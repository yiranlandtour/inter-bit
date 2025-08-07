use super::{PoolInfo, Protocol, RoutingError};
use primitive_types::{H256, U256};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct LiquidityAggregator {
    liquidity_sources: Arc<RwLock<HashMap<H256, LiquiditySource>>>,
    price_feeds: Arc<RwLock<HashMap<(H256, H256), PriceFeed>>>,
    slippage_model: SlippageModel,
}

#[derive(Clone)]
struct LiquiditySource {
    pool_address: H256,
    protocol: Protocol,
    reserve_a: U256,
    reserve_b: U256,
    total_supply: U256,
    fee_rate: u32,
    last_update: u64,
}

#[derive(Clone)]
struct PriceFeed {
    price: U256,
    timestamp: u64,
    source: PriceSource,
}

#[derive(Clone)]
enum PriceSource {
    Oracle,
    TWAP,
    Spot,
}

struct SlippageModel {
    base_slippage: f64,
    liquidity_factor: f64,
    size_factor: f64,
}

impl LiquidityAggregator {
    pub fn new() -> Self {
        Self {
            liquidity_sources: Arc::new(RwLock::new(HashMap::new())),
            price_feeds: Arc::new(RwLock::new(HashMap::new())),
            slippage_model: SlippageModel {
                base_slippage: 0.001,
                liquidity_factor: 0.01,
                size_factor: 0.02,
            },
        }
    }

    pub async fn calculate_swap_output(
        &self,
        pool: &PoolInfo,
        amount_in: U256,
    ) -> Result<(U256, U256, (U256, U256)), RoutingError> {
        match pool.protocol {
            Protocol::AmmV2 => self.calculate_amm_v2_output(pool, amount_in).await,
            Protocol::AmmV3 => self.calculate_amm_v3_output(pool, amount_in).await,
            Protocol::StableSwap => self.calculate_stable_swap_output(pool, amount_in).await,
            Protocol::ConcentratedLiquidity => self.calculate_concentrated_output(pool, amount_in).await,
            Protocol::OrderBook => self.calculate_orderbook_output(pool, amount_in).await,
            Protocol::RfqSystem => self.calculate_rfq_output(pool, amount_in).await,
        }
    }

    async fn calculate_amm_v2_output(
        &self,
        pool: &PoolInfo,
        amount_in: U256,
    ) -> Result<(U256, U256, (U256, U256)), RoutingError> {
        let source = self.get_liquidity_source(pool.address).await?;
        
        let amount_in_with_fee = amount_in * U256::from(997u32);
        let numerator = amount_in_with_fee * source.reserve_b;
        let denominator = source.reserve_a * U256::from(1000u32) + amount_in_with_fee;
        
        if denominator == U256::zero() {
            return Err(RoutingError::InsufficientLiquidity);
        }
        
        let amount_out = numerator / denominator;
        let fee = amount_in * U256::from(3u32) / U256::from(1000u32);
        
        Ok((amount_out, fee, (source.reserve_a, source.reserve_b)))
    }

    async fn calculate_amm_v3_output(
        &self,
        pool: &PoolInfo,
        amount_in: U256,
    ) -> Result<(U256, U256, (U256, U256)), RoutingError> {
        let source = self.get_liquidity_source(pool.address).await?;
        
        let sqrt_price_x96 = self.calculate_sqrt_price(source.reserve_a, source.reserve_b);
        
        let liquidity = self.calculate_liquidity_v3(
            source.reserve_a,
            source.reserve_b,
            sqrt_price_x96,
        );
        
        let sqrt_price_target = self.get_sqrt_price_target(
            sqrt_price_x96,
            liquidity,
            amount_in,
            true,
        );
        
        let amount_out = self.calculate_output_from_price_change(
            liquidity,
            sqrt_price_x96,
            sqrt_price_target,
            false,
        );
        
        let fee = amount_in * U256::from(pool.fee_tier) / U256::from(1_000_000u32);
        
        Ok((amount_out, fee, (source.reserve_a, source.reserve_b)))
    }

    async fn calculate_stable_swap_output(
        &self,
        pool: &PoolInfo,
        amount_in: U256,
    ) -> Result<(U256, U256, (U256, U256)), RoutingError> {
        let source = self.get_liquidity_source(pool.address).await?;
        
        let amp = U256::from(100u32);
        
        let d = self.calculate_d(source.reserve_a, source.reserve_b, amp);
        
        let new_reserve_a = source.reserve_a + amount_in;
        let new_reserve_b = self.calculate_y(new_reserve_a, d, amp);
        
        if new_reserve_b >= source.reserve_b {
            return Err(RoutingError::InsufficientLiquidity);
        }
        
        let amount_out = source.reserve_b - new_reserve_b;
        
        let fee = amount_out * U256::from(4u32) / U256::from(10000u32);
        let amount_out = amount_out - fee;
        
        Ok((amount_out, fee, (source.reserve_a, source.reserve_b)))
    }

    async fn calculate_concentrated_output(
        &self,
        pool: &PoolInfo,
        amount_in: U256,
    ) -> Result<(U256, U256, (U256, U256)), RoutingError> {
        let source = self.get_liquidity_source(pool.address).await?;
        
        let tick_spacing = 60i32;
        let current_tick = self.calculate_tick_from_price(source.reserve_a, source.reserve_b);
        
        let mut remaining = amount_in;
        let mut amount_out = U256::zero();
        let mut current_liquidity = pool.liquidity;
        
        let ticks = self.get_initialized_ticks(pool.address, current_tick, tick_spacing).await;
        
        for tick in ticks {
            if remaining == U256::zero() {
                break;
            }
            
            let tick_liquidity = self.get_tick_liquidity(tick);
            let price_at_tick = self.tick_to_price(tick);
            
            let swap_amount = remaining.min(tick_liquidity);
            let output = self.calculate_swap_within_tick(
                swap_amount,
                current_liquidity,
                price_at_tick,
            );
            
            amount_out = amount_out + output;
            remaining = remaining - swap_amount;
            current_liquidity = current_liquidity + tick_liquidity;
        }
        
        let fee = amount_in * U256::from(pool.fee_tier) / U256::from(1_000_000u32);
        
        Ok((amount_out, fee, (source.reserve_a, source.reserve_b)))
    }

    async fn calculate_orderbook_output(
        &self,
        pool: &PoolInfo,
        amount_in: U256,
    ) -> Result<(U256, U256, (U256, U256)), RoutingError> {
        let asks = self.get_orderbook_asks(pool.address).await;
        
        let mut remaining = amount_in;
        let mut total_output = U256::zero();
        let mut total_cost = U256::zero();
        
        for (price, size) in asks {
            if remaining == U256::zero() {
                break;
            }
            
            let fill_amount = remaining.min(size);
            let cost = fill_amount * price / U256::from(10u64.pow(18));
            
            total_output = total_output + fill_amount;
            total_cost = total_cost + cost;
            remaining = remaining - fill_amount;
        }
        
        if remaining > U256::zero() {
            return Err(RoutingError::InsufficientLiquidity);
        }
        
        let fee = total_cost * U256::from(pool.fee_tier) / U256::from(1_000_000u32);
        
        Ok((total_output, fee, (pool.liquidity, pool.liquidity)))
    }

    async fn calculate_rfq_output(
        &self,
        pool: &PoolInfo,
        amount_in: U256,
    ) -> Result<(U256, U256, (U256, U256)), RoutingError> {
        let quotes = self.request_quotes(pool.address, amount_in).await;
        
        if quotes.is_empty() {
            return Err(RoutingError::NoPoolsAvailable);
        }
        
        let best_quote = quotes.into_iter()
            .max_by_key(|q| q.output_amount)
            .unwrap();
        
        let fee = amount_in * U256::from(pool.fee_tier) / U256::from(1_000_000u32);
        
        Ok((best_quote.output_amount, fee, (pool.liquidity, pool.liquidity)))
    }

    async fn get_liquidity_source(&self, pool_address: H256) -> Result<LiquiditySource, RoutingError> {
        let sources = self.liquidity_sources.read().await;
        sources.get(&pool_address)
            .cloned()
            .ok_or(RoutingError::PoolNotFound)
    }

    fn calculate_sqrt_price(&self, reserve_a: U256, reserve_b: U256) -> U256 {
        if reserve_a == U256::zero() || reserve_b == U256::zero() {
            return U256::from(1u64 << 96);
        }
        
        let ratio = (reserve_b * U256::from(1u64 << 96)) / reserve_a;
        self.sqrt(ratio)
    }

    fn sqrt(&self, value: U256) -> U256 {
        if value == U256::zero() {
            return U256::zero();
        }
        
        let mut x = value;
        let mut y = (x + U256::one()) / U256::from(2u32);
        
        while y < x {
            x = y;
            y = (x + value / x) / U256::from(2u32);
        }
        
        x
    }

    fn calculate_liquidity_v3(&self, reserve_a: U256, reserve_b: U256, sqrt_price: U256) -> U256 {
        if sqrt_price == U256::zero() {
            return U256::zero();
        }
        
        let liquidity_a = reserve_a * sqrt_price / U256::from(1u64 << 96);
        let liquidity_b = reserve_b * U256::from(1u64 << 96) / sqrt_price;
        
        liquidity_a.min(liquidity_b)
    }

    fn get_sqrt_price_target(
        &self,
        sqrt_price_current: U256,
        liquidity: U256,
        amount: U256,
        zero_for_one: bool,
    ) -> U256 {
        if liquidity == U256::zero() {
            return sqrt_price_current;
        }
        
        if zero_for_one {
            let delta = (amount * U256::from(1u64 << 96)) / liquidity;
            sqrt_price_current.saturating_sub(delta)
        } else {
            let delta = (amount * U256::from(1u64 << 96)) / liquidity;
            sqrt_price_current + delta
        }
    }

    fn calculate_output_from_price_change(
        &self,
        liquidity: U256,
        sqrt_price_start: U256,
        sqrt_price_end: U256,
        zero_for_one: bool,
    ) -> U256 {
        if sqrt_price_start == sqrt_price_end {
            return U256::zero();
        }
        
        let delta = if sqrt_price_start > sqrt_price_end {
            sqrt_price_start - sqrt_price_end
        } else {
            sqrt_price_end - sqrt_price_start
        };
        
        (liquidity * delta) / U256::from(1u64 << 96)
    }

    fn calculate_d(&self, x: U256, y: U256, amp: U256) -> U256 {
        let sum = x + y;
        if sum == U256::zero() {
            return U256::zero();
        }
        
        let mut d = sum;
        let mut d_prev;
        
        for _ in 0..255 {
            d_prev = d;
            let d_squared = d * d;
            let d_cubed = d_squared * d;
            
            let numerator = d_cubed + amp * sum * sum;
            let denominator = U256::from(2u32) * d_squared + amp * sum - d;
            
            if denominator == U256::zero() {
                break;
            }
            
            d = numerator / denominator;
            
            if d > d_prev {
                if d - d_prev <= U256::one() {
                    break;
                }
            } else if d_prev - d <= U256::one() {
                break;
            }
        }
        
        d
    }

    fn calculate_y(&self, x: U256, d: U256, amp: U256) -> U256 {
        let mut y = d;
        let mut y_prev;
        
        for _ in 0..255 {
            y_prev = y;
            
            let y_squared = y * y;
            let numerator = y_squared + d * d / (U256::from(2u32) * x);
            let denominator = U256::from(2u32) * y + d / amp - d;
            
            if denominator == U256::zero() {
                break;
            }
            
            y = numerator / denominator;
            
            if y > y_prev {
                if y - y_prev <= U256::one() {
                    break;
                }
            } else if y_prev - y <= U256::one() {
                break;
            }
        }
        
        y
    }

    fn calculate_tick_from_price(&self, reserve_a: U256, reserve_b: U256) -> i32 {
        if reserve_a == U256::zero() || reserve_b == U256::zero() {
            return 0;
        }
        
        let price_ratio = (reserve_b * U256::from(10u64.pow(18))) / reserve_a;
        
        ((price_ratio.as_u128() as f64).ln() / 1.0001_f64.ln()) as i32
    }

    fn tick_to_price(&self, tick: i32) -> U256 {
        let price = 1.0001_f64.powi(tick);
        U256::from((price * 10_f64.powi(18)) as u128)
    }

    async fn get_initialized_ticks(&self, _pool: H256, _current: i32, _spacing: i32) -> Vec<i32> {
        vec![-100, -60, 0, 60, 100]
    }

    fn get_tick_liquidity(&self, _tick: i32) -> U256 {
        U256::from(1_000_000u64)
    }

    fn calculate_swap_within_tick(&self, amount: U256, liquidity: U256, _price: U256) -> U256 {
        if liquidity == U256::zero() {
            return U256::zero();
        }
        
        amount * U256::from(997u32) / U256::from(1000u32)
    }

    async fn get_orderbook_asks(&self, _pool: H256) -> Vec<(U256, U256)> {
        vec![
            (U256::from(1000u64), U256::from(100_000u64)),
            (U256::from(1001u64), U256::from(200_000u64)),
            (U256::from(1002u64), U256::from(300_000u64)),
        ]
    }

    async fn request_quotes(&self, _pool: H256, _amount: U256) -> Vec<Quote> {
        vec![
            Quote {
                provider: H256::random(),
                output_amount: U256::from(990_000u64),
                expiry: 1000,
            },
        ]
    }

    pub async fn update_liquidity_source(&self, pool: H256, source: LiquiditySource) {
        let mut sources = self.liquidity_sources.write().await;
        sources.insert(pool, source);
    }

    pub async fn update_price_feed(&self, token_a: H256, token_b: H256, price: U256) {
        let mut feeds = self.price_feeds.write().await;
        feeds.insert((token_a, token_b), PriceFeed {
            price,
            timestamp: self.get_current_timestamp(),
            source: PriceSource::Spot,
        });
    }

    fn get_current_timestamp(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }
}

struct Quote {
    provider: H256,
    output_amount: U256,
    expiry: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_amm_v2_calculation() {
        let aggregator = LiquidityAggregator::new();
        
        let pool = PoolInfo {
            address: H256::random(),
            protocol: Protocol::AmmV2,
            token_a: H256::random(),
            token_b: H256::random(),
            fee_tier: 3000,
            liquidity: U256::from(1_000_000u64),
            volume_24h: U256::zero(),
            tvl: U256::zero(),
        };
        
        aggregator.update_liquidity_source(
            pool.address,
            LiquiditySource {
                pool_address: pool.address,
                protocol: Protocol::AmmV2,
                reserve_a: U256::from(1_000_000u64),
                reserve_b: U256::from(2_000_000u64),
                total_supply: U256::from(1_000_000u64),
                fee_rate: 3000,
                last_update: 0,
            },
        ).await;
        
        let result = aggregator.calculate_swap_output(&pool, U256::from(1000u64)).await;
        assert!(result.is_ok());
        
        let (output, fee, _reserves) = result.unwrap();
        assert!(output > U256::zero());
        assert!(fee > U256::zero());
    }
}