// 自动做市商(AMM)池实现
// 支持Uniswap V2/V3风格的流动性池

use super::{DexError, LiquidityResult};
use primitive_types::{H256, U256};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// AMM流动性池
pub struct AmmPool {
    token_a: H256,
    token_b: H256,
    reserve_a: Arc<RwLock<U256>>,
    reserve_b: Arc<RwLock<U256>>,
    total_supply: Arc<RwLock<U256>>, // LP代币总供应量
    liquidity_providers: Arc<RwLock<HashMap<[u8; 20], U256>>>,
    fee_rate: U256, // 基点表示，例如30 = 0.3%
    
    // Uniswap V3风格的集中流动性
    concentrated_liquidity: Arc<RwLock<ConcentratedLiquidity>>,
    
    // 价格预言机
    price_oracle: Arc<RwLock<PriceOracle>>,
}

/// 集中流动性管理
struct ConcentratedLiquidity {
    tick_spacing: i32,
    current_tick: i32,
    liquidity: U256,
    positions: HashMap<[u8; 20], LiquidityPosition>,
}

/// 流动性位置
#[derive(Clone)]
struct LiquidityPosition {
    owner: [u8; 20],
    tick_lower: i32,
    tick_upper: i32,
    liquidity: U256,
    fee_growth_inside_0: U256,
    fee_growth_inside_1: U256,
}

/// 价格预言机
struct PriceOracle {
    price_cumulative: U256,
    last_update: u64,
    twap_window: u64, // TWAP窗口期
}

impl AmmPool {
    pub fn new(token_a: H256, token_b: H256) -> Self {
        Self {
            token_a,
            token_b,
            reserve_a: Arc::new(RwLock::new(U256::zero())),
            reserve_b: Arc::new(RwLock::new(U256::zero())),
            total_supply: Arc::new(RwLock::new(U256::zero())),
            liquidity_providers: Arc::new(RwLock::new(HashMap::new())),
            fee_rate: U256::from(30), // 0.3%默认费率
            concentrated_liquidity: Arc::new(RwLock::new(ConcentratedLiquidity {
                tick_spacing: 60,
                current_tick: 0,
                liquidity: U256::zero(),
                positions: HashMap::new(),
            })),
            price_oracle: Arc::new(RwLock::new(PriceOracle {
                price_cumulative: U256::zero(),
                last_update: 0,
                twap_window: 3600, // 1小时TWAP
            })),
        }
    }

    /// 添加流动性
    pub async fn add_liquidity(
        &self,
        amount_a: U256,
        amount_b: U256,
        provider: [u8; 20],
    ) -> Result<LiquidityResult, DexError> {
        let mut reserve_a = self.reserve_a.write().await;
        let mut reserve_b = self.reserve_b.write().await;
        let mut total_supply = self.total_supply.write().await;
        let mut providers = self.liquidity_providers.write().await;

        let lp_tokens = if *total_supply == U256::zero() {
            // 初始流动性
            let initial_liquidity = self.sqrt(amount_a * amount_b);
            *reserve_a = amount_a;
            *reserve_b = amount_b;
            *total_supply = initial_liquidity;
            initial_liquidity
        } else {
            // 计算应该添加的代币比例
            let liquidity_a = amount_a * *total_supply / *reserve_a;
            let liquidity_b = amount_b * *total_supply / *reserve_b;
            let liquidity = liquidity_a.min(liquidity_b);

            // 实际添加的数量
            let actual_a = liquidity * *reserve_a / *total_supply;
            let actual_b = liquidity * *reserve_b / *total_supply;

            *reserve_a += actual_a;
            *reserve_b += actual_b;
            *total_supply += liquidity;

            liquidity
        };

        // 更新流动性提供者份额
        *providers.entry(provider).or_insert(U256::zero()) += lp_tokens;

        // 更新价格预言机
        self.update_oracle().await;

        Ok(LiquidityResult {
            pool_id: self.get_pool_id(),
            lp_tokens,
            share: (lp_tokens.as_u128() as f64) / (total_supply.as_u128() as f64),
        })
    }

    /// 移除流动性
    pub async fn remove_liquidity(
        &self,
        lp_tokens: U256,
        provider: [u8; 20],
    ) -> Result<(U256, U256), DexError> {
        let mut reserve_a = self.reserve_a.write().await;
        let mut reserve_b = self.reserve_b.write().await;
        let mut total_supply = self.total_supply.write().await;
        let mut providers = self.liquidity_providers.write().await;

        // 检查LP代币余额
        let balance = providers.get(&provider).cloned().unwrap_or(U256::zero());
        if balance < lp_tokens {
            return Err(DexError::InsufficientBalance);
        }

        // 计算应得的代币数量
        let amount_a = lp_tokens * *reserve_a / *total_supply;
        let amount_b = lp_tokens * *reserve_b / *total_supply;

        // 更新储备
        *reserve_a -= amount_a;
        *reserve_b -= amount_b;
        *total_supply -= lp_tokens;

        // 更新提供者余额
        *providers.get_mut(&provider).unwrap() -= lp_tokens;

        // 更新价格预言机
        self.update_oracle().await;

        Ok((amount_a, amount_b))
    }

    /// 交换代币 (恒定乘积公式: x * y = k)
    pub async fn swap(
        &self,
        amount_in: U256,
        token_in: H256,
    ) -> Result<U256, DexError> {
        let mut reserve_a = self.reserve_a.write().await;
        let mut reserve_b = self.reserve_b.write().await;

        let (reserve_in, reserve_out) = if token_in == self.token_a {
            (*reserve_a, *reserve_b)
        } else if token_in == self.token_b {
            (*reserve_b, *reserve_a)
        } else {
            return Err(DexError::InvalidAmount);
        };

        // 计算输出数量 (扣除手续费)
        let amount_in_with_fee = amount_in * (U256::from(10000) - self.fee_rate) / U256::from(10000);
        let numerator = amount_in_with_fee * reserve_out;
        let denominator = reserve_in + amount_in_with_fee;
        let amount_out = numerator / denominator;

        if amount_out == U256::zero() {
            return Err(DexError::InsufficientLiquidity);
        }

        // 更新储备
        if token_in == self.token_a {
            *reserve_a += amount_in;
            *reserve_b -= amount_out;
        } else {
            *reserve_b += amount_in;
            *reserve_a -= amount_out;
        }

        // 更新价格预言机
        self.update_oracle().await;

        Ok(amount_out)
    }

    /// 获取输出数量预估
    pub fn get_amount_out(&self, amount_in: U256) -> Result<U256, DexError> {
        // 这是一个只读版本的swap计算
        let reserve_in = U256::from(1000000); // 示例值
        let reserve_out = U256::from(1000000); // 示例值

        let amount_in_with_fee = amount_in * (U256::from(10000) - self.fee_rate) / U256::from(10000);
        let numerator = amount_in_with_fee * reserve_out;
        let denominator = reserve_in + amount_in_with_fee;
        
        Ok(numerator / denominator)
    }

    /// 添加集中流动性位置 (Uniswap V3风格)
    pub async fn add_concentrated_liquidity(
        &self,
        provider: [u8; 20],
        tick_lower: i32,
        tick_upper: i32,
        liquidity: U256,
    ) -> Result<(), DexError> {
        let mut cl = self.concentrated_liquidity.write().await;

        // 验证tick范围
        if tick_lower >= tick_upper {
            return Err(DexError::InvalidPrice);
        }

        if tick_lower % cl.tick_spacing != 0 || tick_upper % cl.tick_spacing != 0 {
            return Err(DexError::InvalidPrice);
        }

        // 创建或更新位置
        let position = LiquidityPosition {
            owner: provider,
            tick_lower,
            tick_upper,
            liquidity,
            fee_growth_inside_0: U256::zero(),
            fee_growth_inside_1: U256::zero(),
        };

        cl.positions.insert(provider, position);

        // 如果当前价格在范围内，更新活跃流动性
        if tick_lower <= cl.current_tick && cl.current_tick < tick_upper {
            cl.liquidity += liquidity;
        }

        Ok(())
    }

    /// 获取当前价格
    pub async fn get_price(&self) -> U256 {
        let reserve_a = *self.reserve_a.read().await;
        let reserve_b = *self.reserve_b.read().await;

        if reserve_a == U256::zero() {
            return U256::zero();
        }

        reserve_b * U256::from(10u64.pow(18)) / reserve_a
    }

    /// 获取TWAP价格
    pub async fn get_twap(&self) -> U256 {
        let oracle = self.price_oracle.read().await;
        let current_time = self.current_timestamp();
        let time_elapsed = current_time - oracle.last_update;

        if time_elapsed < oracle.twap_window {
            return U256::zero(); // TWAP窗口期未满
        }

        oracle.price_cumulative / U256::from(time_elapsed)
    }

    /// 更新价格预言机
    async fn update_oracle(&self) {
        let mut oracle = self.price_oracle.write().await;
        let current_time = self.current_timestamp();
        let time_elapsed = current_time - oracle.last_update;

        if time_elapsed > 0 {
            let current_price = self.get_price().await;
            oracle.price_cumulative += current_price * U256::from(time_elapsed);
            oracle.last_update = current_time;
        }
    }

    /// 计算价格影响
    pub async fn calculate_price_impact(&self, amount_in: U256, token_in: H256) -> f64 {
        let reserve_a = *self.reserve_a.read().await;
        let reserve_b = *self.reserve_b.read().await;

        let (reserve_in, reserve_out) = if token_in == self.token_a {
            (reserve_a, reserve_b)
        } else {
            (reserve_b, reserve_a)
        };

        let price_before = reserve_out.as_u128() as f64 / reserve_in.as_u128() as f64;
        
        // 计算交换后的储备
        let amount_out = self.get_amount_out(amount_in).unwrap_or(U256::zero());
        let new_reserve_in = reserve_in + amount_in;
        let new_reserve_out = reserve_out - amount_out;
        
        let price_after = new_reserve_out.as_u128() as f64 / new_reserve_in.as_u128() as f64;
        
        ((price_before - price_after) / price_before * 100.0).abs()
    }

    /// 计算滑点
    pub async fn calculate_slippage(&self, amount_in: U256, expected_out: U256) -> f64 {
        let actual_out = self.get_amount_out(amount_in).unwrap_or(U256::zero());
        
        if expected_out == U256::zero() {
            return 0.0;
        }

        let slippage = if actual_out < expected_out {
            (expected_out - actual_out).as_u128() as f64
        } else {
            0.0
        };

        (slippage / expected_out.as_u128() as f64) * 100.0
    }

    /// 平方根函数 (用于初始流动性计算)
    fn sqrt(&self, x: U256) -> U256 {
        if x == U256::zero() {
            return U256::zero();
        }

        let mut z = x;
        let mut y = (x + U256::one()) / U256::from(2);

        while y < z {
            z = y;
            y = (x / y + y) / U256::from(2);
        }

        z
    }

    fn get_pool_id(&self) -> H256 {
        use sha2::{Digest, Sha256};
        
        let mut hasher = Sha256::new();
        hasher.update(self.token_a.as_bytes());
        hasher.update(self.token_b.as_bytes());
        
        H256::from_slice(&hasher.finalize())
    }

    fn current_timestamp(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }
}

/// 流动性池信息
#[derive(Debug, Clone)]
pub struct PoolInfo {
    pub token_a: H256,
    pub token_b: H256,
    pub reserve_a: U256,
    pub reserve_b: U256,
    pub total_supply: U256,
    pub fee_rate: U256,
    pub volume_24h: U256,
    pub tvl: U256, // Total Value Locked
}