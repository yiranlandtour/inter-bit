use super::{DeFiConfig, DeFiError};
use primitive_types::{H256, U256};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct PerpetualEngine {
    config: DeFiConfig,
    markets: Arc<RwLock<HashMap<H256, PerpetualMarket>>>,
    positions: Arc<RwLock<HashMap<H256, Position>>>,
    funding_rates: Arc<RwLock<HashMap<H256, FundingRate>>>,
    insurance_fund: Arc<RwLock<InsuranceFund>>,
    oracle: Arc<PriceOracle>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerpetualMarket {
    pub id: H256,
    pub base_token: H256,
    pub quote_token: H256,
    pub index_price: U256,
    pub mark_price: U256,
    pub open_interest_long: U256,
    pub open_interest_short: U256,
    pub max_leverage: u32,
    pub maintenance_margin_ratio: u32, // basis points
    pub initial_margin_ratio: u32,     // basis points
    pub funding_rate: i64,              // can be negative
    pub next_funding_time: u64,
    pub is_active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub id: H256,
    pub trader: [u8; 20],
    pub market_id: H256,
    pub size: U256,           // position size in base token
    pub collateral: U256,     // collateral in quote token
    pub entry_price: U256,
    pub leverage: u32,
    pub is_long: bool,
    pub realized_pnl: i128,
    pub unrealized_pnl: i128,
    pub funding_paid: i128,
    pub liquidation_price: U256,
    pub created_at: u64,
    pub last_funding_update: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FundingRate {
    pub market_id: H256,
    pub rate: i64,
    pub timestamp: u64,
    pub long_open_interest: U256,
    pub short_open_interest: U256,
}

struct InsuranceFund {
    balance: U256,
    total_contributions: U256,
    total_payouts: U256,
}

struct PriceOracle {
    prices: Arc<RwLock<HashMap<H256, OraclePrice>>>,
}

#[derive(Clone)]
struct OraclePrice {
    price: U256,
    timestamp: u64,
    confidence: u32,
}

impl PerpetualEngine {
    pub fn new(config: DeFiConfig) -> Self {
        Self {
            config,
            markets: Arc::new(RwLock::new(HashMap::new())),
            positions: Arc::new(RwLock::new(HashMap::new())),
            funding_rates: Arc::new(RwLock::new(HashMap::new())),
            insurance_fund: Arc::new(RwLock::new(InsuranceFund {
                balance: U256::zero(),
                total_contributions: U256::zero(),
                total_payouts: U256::zero(),
            })),
            oracle: Arc::new(PriceOracle {
                prices: Arc::new(RwLock::new(HashMap::new())),
            }),
        }
    }

    pub async fn open_position(
        &self,
        trader: [u8; 20],
        market_id: H256,
        size: U256,
        leverage: u32,
        is_long: bool,
    ) -> Result<H256, DeFiError> {
        let markets = self.markets.read().await;
        let market = markets.get(&market_id).ok_or(DeFiError::MarketClosed)?;

        if !market.is_active {
            return Err(DeFiError::MarketClosed);
        }

        if leverage > market.max_leverage {
            return Err(DeFiError::ExcessiveLeverage);
        }

        let collateral_required = self.calculate_required_collateral(
            size,
            market.mark_price,
            leverage,
            market.initial_margin_ratio,
        );

        let liquidation_price = self.calculate_liquidation_price(
            market.mark_price,
            leverage,
            is_long,
            market.maintenance_margin_ratio,
        );

        let position_id = H256::random();
        let position = Position {
            id: position_id,
            trader,
            market_id,
            size,
            collateral: collateral_required,
            entry_price: market.mark_price,
            leverage,
            is_long,
            realized_pnl: 0,
            unrealized_pnl: 0,
            funding_paid: 0,
            liquidation_price,
            created_at: self.get_current_timestamp(),
            last_funding_update: self.get_current_timestamp(),
        };

        drop(markets);

        // Update open interest
        let mut markets = self.markets.write().await;
        if let Some(market) = markets.get_mut(&market_id) {
            if is_long {
                market.open_interest_long = market.open_interest_long + size;
            } else {
                market.open_interest_short = market.open_interest_short + size;
            }
        }

        let mut positions = self.positions.write().await;
        positions.insert(position_id, position);

        // Update funding rate based on new open interest
        self.update_funding_rate(market_id).await?;

        Ok(position_id)
    }

    pub async fn close_position(
        &self,
        trader: [u8; 20],
        position_id: H256,
    ) -> Result<ClosedPositionResult, DeFiError> {
        let mut positions = self.positions.write().await;
        let position = positions.get(&position_id).ok_or(DeFiError::PositionNotFound)?;

        if position.trader != trader {
            return Err(DeFiError::Unauthorized);
        }

        let markets = self.markets.read().await;
        let market = markets.get(&position.market_id).ok_or(DeFiError::MarketClosed)?;

        // Calculate PnL
        let pnl = self.calculate_pnl(position, market.mark_price);
        
        // Apply funding
        let funding = self.calculate_accumulated_funding(position, market).await;

        let final_pnl = pnl - funding;
        let return_amount = position.collateral.as_u128() as i128 + final_pnl;

        // Update open interest
        drop(markets);
        let mut markets = self.markets.write().await;
        if let Some(market) = markets.get_mut(&position.market_id) {
            if position.is_long {
                market.open_interest_long = market.open_interest_long.saturating_sub(position.size);
            } else {
                market.open_interest_short = market.open_interest_short.saturating_sub(position.size);
            }
        }

        positions.remove(&position_id);

        Ok(ClosedPositionResult {
            position_id,
            realized_pnl: final_pnl,
            funding_paid: funding,
            return_amount: if return_amount > 0 {
                U256::from(return_amount as u128)
            } else {
                U256::zero()
            },
        })
    }

    pub async fn liquidate_position(
        &self,
        liquidator: [u8; 20],
        position_id: H256,
    ) -> Result<U256, DeFiError> {
        let positions = self.positions.read().await;
        let position = positions.get(&position_id).ok_or(DeFiError::PositionNotFound)?;

        let markets = self.markets.read().await;
        let market = markets.get(&position.market_id).ok_or(DeFiError::MarketClosed)?;

        // Check if position is liquidatable
        let is_liquidatable = if position.is_long {
            market.mark_price <= position.liquidation_price
        } else {
            market.mark_price >= position.liquidation_price
        };

        if !is_liquidatable {
            return Err(DeFiError::PositionHealthy);
        }

        let liquidation_value = position.collateral;
        let liquidation_fee = liquidation_value * U256::from(self.config.liquidation_penalty_bps) 
            / U256::from(10000);

        // Pay liquidator
        let liquidator_reward = liquidation_fee / U256::from(2);
        
        // Rest goes to insurance fund
        let insurance_contribution = liquidation_fee - liquidator_reward;
        
        drop(positions);
        drop(markets);

        let mut insurance = self.insurance_fund.write().await;
        insurance.balance = insurance.balance + insurance_contribution;
        insurance.total_contributions = insurance.total_contributions + insurance_contribution;

        // Remove position
        let mut positions = self.positions.write().await;
        positions.remove(&position_id);

        Ok(liquidator_reward)
    }

    pub async fn add_collateral(
        &self,
        trader: [u8; 20],
        position_id: H256,
        amount: U256,
    ) -> Result<(), DeFiError> {
        let mut positions = self.positions.write().await;
        let position = positions.get_mut(&position_id).ok_or(DeFiError::PositionNotFound)?;

        if position.trader != trader {
            return Err(DeFiError::Unauthorized);
        }

        position.collateral = position.collateral + amount;
        
        // Recalculate liquidation price with new collateral
        let markets = self.markets.read().await;
        let market = markets.get(&position.market_id).ok_or(DeFiError::MarketClosed)?;
        
        position.liquidation_price = self.calculate_liquidation_price(
            position.entry_price,
            position.leverage,
            position.is_long,
            market.maintenance_margin_ratio,
        );

        Ok(())
    }

    pub async fn get_position_value(&self, trader: [u8; 20]) -> Result<U256, DeFiError> {
        let positions = self.positions.read().await;
        let markets = self.markets.read().await;
        
        let mut total_value = U256::zero();
        
        for position in positions.values() {
            if position.trader == trader {
                if let Some(market) = markets.get(&position.market_id) {
                    let pnl = self.calculate_pnl(position, market.mark_price);
                    let position_value = position.collateral.as_u128() as i128 + pnl;
                    
                    if position_value > 0 {
                        total_value = total_value + U256::from(position_value as u128);
                    }
                }
            }
        }
        
        Ok(total_value)
    }

    async fn update_funding_rate(&self, market_id: H256) -> Result<(), DeFiError> {
        let mut markets = self.markets.write().await;
        let market = markets.get_mut(&market_id).ok_or(DeFiError::MarketClosed)?;

        let current_time = self.get_current_timestamp();
        
        if current_time >= market.next_funding_time {
            // Calculate funding rate based on open interest imbalance
            let long_oi = market.open_interest_long.as_u128() as i128;
            let short_oi = market.open_interest_short.as_u128() as i128;
            let imbalance = long_oi - short_oi;
            
            // Funding rate = imbalance / total_oi * 0.01% per funding interval
            let total_oi = long_oi + short_oi;
            if total_oi > 0 {
                market.funding_rate = (imbalance * 100 / total_oi) as i64; // basis points
            } else {
                market.funding_rate = 0;
            }

            market.next_funding_time = current_time + self.config.funding_interval;

            // Store funding rate history
            let mut funding_rates = self.funding_rates.write().await;
            funding_rates.insert(
                H256::random(),
                FundingRate {
                    market_id,
                    rate: market.funding_rate,
                    timestamp: current_time,
                    long_open_interest: market.open_interest_long,
                    short_open_interest: market.open_interest_short,
                },
            );
        }

        Ok(())
    }

    async fn calculate_accumulated_funding(
        &self,
        position: &Position,
        market: &PerpetualMarket,
    ) -> i128 {
        let time_elapsed = self.get_current_timestamp() - position.last_funding_update;
        let funding_periods = time_elapsed / self.config.funding_interval;
        
        let funding_rate = market.funding_rate;
        let position_value = (position.size * market.mark_price).as_u128() as i128;
        
        // Long positions pay when funding is positive, receive when negative
        // Short positions receive when funding is positive, pay when negative
        let funding = if position.is_long {
            -(position_value * funding_rate as i128 * funding_periods as i128) / 10000
        } else {
            (position_value * funding_rate as i128 * funding_periods as i128) / 10000
        };
        
        funding
    }

    fn calculate_pnl(&self, position: &Position, current_price: U256) -> i128 {
        let entry_value = (position.size * position.entry_price).as_u128() as i128;
        let current_value = (position.size * current_price).as_u128() as i128;
        
        if position.is_long {
            current_value - entry_value
        } else {
            entry_value - current_value
        }
    }

    fn calculate_required_collateral(
        &self,
        size: U256,
        price: U256,
        leverage: u32,
        initial_margin_ratio: u32,
    ) -> U256 {
        let position_value = size * price;
        let min_collateral = position_value / U256::from(leverage);
        let initial_margin = position_value * U256::from(initial_margin_ratio) / U256::from(10000);
        
        min_collateral.max(initial_margin)
    }

    fn calculate_liquidation_price(
        &self,
        entry_price: U256,
        leverage: u32,
        is_long: bool,
        maintenance_margin_ratio: u32,
    ) -> U256 {
        let maintenance_margin_pct = U256::from(maintenance_margin_ratio) * U256::from(100) 
            / U256::from(10000);
        
        if is_long {
            // Liquidation when: price <= entry_price * (1 - 1/leverage + maintenance_margin%)
            let factor = U256::from(10000) - (U256::from(10000) / U256::from(leverage)) 
                + maintenance_margin_pct;
            entry_price * factor / U256::from(10000)
        } else {
            // Liquidation when: price >= entry_price * (1 + 1/leverage - maintenance_margin%)
            let factor = U256::from(10000) + (U256::from(10000) / U256::from(leverage)) 
                - maintenance_margin_pct;
            entry_price * factor / U256::from(10000)
        }
    }

    fn get_current_timestamp(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    pub async fn create_market(
        &self,
        base_token: H256,
        quote_token: H256,
        max_leverage: u32,
    ) -> Result<H256, DeFiError> {
        let market_id = H256::random();
        
        let market = PerpetualMarket {
            id: market_id,
            base_token,
            quote_token,
            index_price: U256::from(1000 * 10u64.pow(18)), // $1000 initial price
            mark_price: U256::from(1000 * 10u64.pow(18)),
            open_interest_long: U256::zero(),
            open_interest_short: U256::zero(),
            max_leverage,
            maintenance_margin_ratio: 250, // 2.5%
            initial_margin_ratio: 500,     // 5%
            funding_rate: 0,
            next_funding_time: self.get_current_timestamp() + self.config.funding_interval,
            is_active: true,
        };

        let mut markets = self.markets.write().await;
        markets.insert(market_id, market);

        Ok(market_id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClosedPositionResult {
    pub position_id: H256,
    pub realized_pnl: i128,
    pub funding_paid: i128,
    pub return_amount: U256,
}

impl PriceOracle {
    pub async fn update_price(&self, token: H256, price: U256) {
        let mut prices = self.prices.write().await;
        prices.insert(
            token,
            OraclePrice {
                price,
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
                confidence: 99,
            },
        );
    }

    pub async fn get_price(&self, token: H256) -> Option<U256> {
        let prices = self.prices.read().await;
        prices.get(&token).map(|p| p.price)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_perpetual_position_lifecycle() {
        let config = DeFiConfig::default();
        let engine = PerpetualEngine::new(config);
        
        // Create a market
        let market_id = engine.create_market(
            H256::random(), // BTC
            H256::random(), // USDC
            50,
        ).await.unwrap();

        // Open a long position
        let trader = [1u8; 20];
        let position_id = engine.open_position(
            trader,
            market_id,
            U256::from(10u64.pow(18)), // 1 BTC
            10, // 10x leverage
            true, // long
        ).await.unwrap();

        assert_ne!(position_id, H256::zero());

        // Close the position
        let result = engine.close_position(trader, position_id).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_leverage_limits() {
        let config = DeFiConfig::default();
        let engine = PerpetualEngine::new(config);
        
        let market_id = engine.create_market(
            H256::random(),
            H256::random(),
            50,
        ).await.unwrap();

        let trader = [1u8; 20];
        let result = engine.open_position(
            trader,
            market_id,
            U256::from(10u64.pow(18)),
            60, // Exceeds max leverage of 50
            true,
        ).await;

        assert!(matches!(result, Err(DeFiError::ExcessiveLeverage)));
    }
}