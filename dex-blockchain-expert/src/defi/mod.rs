pub mod perpetual;
pub mod flash_loan;
pub mod options;
pub mod lending;
pub mod yield_farming;

use primitive_types::{H256, U256};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeFiConfig {
    pub flash_loan_fee_bps: u32, // basis points (0.09% = 9 bps)
    pub liquidation_penalty_bps: u32,
    pub min_collateral_ratio: u32, // percentage * 100 (150% = 15000)
    pub max_leverage: u32,
    pub funding_interval: u64, // seconds
    pub oracle_price_tolerance: u32, // basis points
}

impl Default for DeFiConfig {
    fn default() -> Self {
        Self {
            flash_loan_fee_bps: 9, // 0.09%
            liquidation_penalty_bps: 500, // 5%
            min_collateral_ratio: 15000, // 150%
            max_leverage: 100, // 100x
            funding_interval: 28800, // 8 hours
            oracle_price_tolerance: 100, // 1%
        }
    }
}

pub struct DeFiEngine {
    config: DeFiConfig,
    perpetual_engine: Arc<perpetual::PerpetualEngine>,
    flash_loan_provider: Arc<flash_loan::FlashLoanProvider>,
    options_protocol: Arc<options::OptionsProtocol>,
    lending_pool: Arc<lending::LendingPool>,
    yield_optimizer: Arc<yield_farming::YieldOptimizer>,
}

impl DeFiEngine {
    pub fn new(config: DeFiConfig) -> Self {
        Self {
            perpetual_engine: Arc::new(perpetual::PerpetualEngine::new(config.clone())),
            flash_loan_provider: Arc::new(flash_loan::FlashLoanProvider::new(
                config.flash_loan_fee_bps,
            )),
            options_protocol: Arc::new(options::OptionsProtocol::new()),
            lending_pool: Arc::new(lending::LendingPool::new(config.clone())),
            yield_optimizer: Arc::new(yield_farming::YieldOptimizer::new()),
            config,
        }
    }

    pub async fn open_perpetual_position(
        &self,
        trader: [u8; 20],
        market: H256,
        size: U256,
        leverage: u32,
        is_long: bool,
    ) -> Result<H256, DeFiError> {
        if leverage > self.config.max_leverage {
            return Err(DeFiError::ExcessiveLeverage);
        }

        self.perpetual_engine
            .open_position(trader, market, size, leverage, is_long)
            .await
    }

    pub async fn execute_flash_loan(
        &self,
        borrower: [u8; 20],
        token: H256,
        amount: U256,
        callback_data: Vec<u8>,
    ) -> Result<H256, DeFiError> {
        self.flash_loan_provider
            .execute_loan(borrower, token, amount, callback_data)
            .await
    }

    pub async fn create_option(
        &self,
        writer: [u8; 20],
        option_type: options::OptionType,
        underlying: H256,
        strike_price: U256,
        expiry: u64,
        amount: U256,
    ) -> Result<H256, DeFiError> {
        self.options_protocol
            .create_option(writer, option_type, underlying, strike_price, expiry, amount)
            .await
    }

    pub async fn deposit_to_lending(
        &self,
        user: [u8; 20],
        token: H256,
        amount: U256,
    ) -> Result<U256, DeFiError> {
        self.lending_pool.deposit(user, token, amount).await
    }

    pub async fn borrow_from_lending(
        &self,
        user: [u8; 20],
        token: H256,
        amount: U256,
        collateral_token: H256,
    ) -> Result<H256, DeFiError> {
        self.lending_pool
            .borrow(user, token, amount, collateral_token)
            .await
    }

    pub async fn stake_for_yield(
        &self,
        user: [u8; 20],
        pool_id: H256,
        amount: U256,
    ) -> Result<(), DeFiError> {
        self.yield_optimizer.stake(user, pool_id, amount).await
    }

    pub async fn calculate_portfolio_value(
        &self,
        user: [u8; 20],
    ) -> Result<PortfolioValue, DeFiError> {
        let perp_value = self.perpetual_engine.get_position_value(user).await?;
        let lending_value = self.lending_pool.get_account_value(user).await?;
        let options_value = self.options_protocol.get_portfolio_value(user).await?;
        let farming_value = self.yield_optimizer.get_staked_value(user).await?;

        Ok(PortfolioValue {
            total_value: perp_value + lending_value + options_value + farming_value,
            perpetual_positions: perp_value,
            lending_positions: lending_value,
            options_positions: options_value,
            yield_farming: farming_value,
        })
    }

    pub async fn liquidate_position(
        &self,
        liquidator: [u8; 20],
        position_id: H256,
    ) -> Result<LiquidationResult, DeFiError> {
        // Check if position is liquidatable
        let health_factor = self.calculate_health_factor(position_id).await?;
        
        if health_factor >= U256::from(10000) { // 100%
            return Err(DeFiError::PositionHealthy);
        }

        // Execute liquidation
        let liquidation_value = self.perpetual_engine
            .liquidate_position(liquidator, position_id)
            .await?;

        let penalty = liquidation_value * U256::from(self.config.liquidation_penalty_bps) 
            / U256::from(10000);

        Ok(LiquidationResult {
            position_id,
            liquidator,
            liquidation_value,
            penalty,
            timestamp: self.get_current_timestamp(),
        })
    }

    async fn calculate_health_factor(&self, position_id: H256) -> Result<U256, DeFiError> {
        // Simplified health factor calculation
        Ok(U256::from(12000)) // 120%
    }

    fn get_current_timestamp(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortfolioValue {
    pub total_value: U256,
    pub perpetual_positions: U256,
    pub lending_positions: U256,
    pub options_positions: U256,
    pub yield_farming: U256,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiquidationResult {
    pub position_id: H256,
    pub liquidator: [u8; 20],
    pub liquidation_value: U256,
    pub penalty: U256,
    pub timestamp: u64,
}

#[derive(Debug)]
pub enum DeFiError {
    InsufficientBalance,
    InsufficientCollateral,
    ExcessiveLeverage,
    PositionNotFound,
    MarketClosed,
    InvalidExpiry,
    FlashLoanNotRepaid,
    PositionHealthy,
    OracleError,
    Unauthorized,
}

impl std::error::Error for DeFiError {}

impl std::fmt::Display for DeFiError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            DeFiError::InsufficientBalance => write!(f, "Insufficient balance"),
            DeFiError::InsufficientCollateral => write!(f, "Insufficient collateral"),
            DeFiError::ExcessiveLeverage => write!(f, "Excessive leverage"),
            DeFiError::PositionNotFound => write!(f, "Position not found"),
            DeFiError::MarketClosed => write!(f, "Market closed"),
            DeFiError::InvalidExpiry => write!(f, "Invalid expiry"),
            DeFiError::FlashLoanNotRepaid => write!(f, "Flash loan not repaid"),
            DeFiError::PositionHealthy => write!(f, "Position is healthy"),
            DeFiError::OracleError => write!(f, "Oracle error"),
            DeFiError::Unauthorized => write!(f, "Unauthorized"),
        }
    }
}