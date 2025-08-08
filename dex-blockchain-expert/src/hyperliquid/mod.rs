pub mod api;
pub mod connector;
pub mod types;
pub mod order_manager;
pub mod market_data;
pub mod vault_integration;

use crate::defi::DeFiError;
use primitive_types::{H256, U256};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyperliquidConfig {
    pub api_endpoint: String,
    pub websocket_endpoint: String,
    pub chain_id: u32,
    pub max_slippage_bps: u32,
    pub default_leverage: u32,
    pub enable_vault_trading: bool,
    pub enable_spot_trading: bool,
    pub api_key: Option<String>,
    pub api_secret: Option<String>,
}

impl Default for HyperliquidConfig {
    fn default() -> Self {
        Self {
            api_endpoint: "https://api.hyperliquid.xyz".to_string(),
            websocket_endpoint: "wss://api.hyperliquid.xyz/ws".to_string(),
            chain_id: 1,
            max_slippage_bps: 50, // 0.5%
            default_leverage: 10,
            enable_vault_trading: true,
            enable_spot_trading: true,
            api_key: None,
            api_secret: None,
        }
    }
}

pub struct HyperliquidIntegration {
    config: HyperliquidConfig,
    api_client: Arc<api::HyperliquidApiClient>,
    connector: Arc<connector::HyperliquidConnector>,
    order_manager: Arc<order_manager::OrderManager>,
    market_data: Arc<market_data::MarketDataProvider>,
    vault_manager: Option<Arc<vault_integration::VaultManager>>,
}

impl HyperliquidIntegration {
    pub async fn new(config: HyperliquidConfig) -> Result<Self, HyperliquidError> {
        let api_client = Arc::new(api::HyperliquidApiClient::new(config.clone())?);
        let connector = Arc::new(connector::HyperliquidConnector::new(api_client.clone()).await?);
        let order_manager = Arc::new(order_manager::OrderManager::new(connector.clone()));
        let market_data = Arc::new(market_data::MarketDataProvider::new(api_client.clone()).await?);
        
        let vault_manager = if config.enable_vault_trading {
            Some(Arc::new(vault_integration::VaultManager::new(connector.clone())))
        } else {
            None
        };

        Ok(Self {
            config,
            api_client,
            connector,
            order_manager,
            market_data,
            vault_manager,
        })
    }

    pub async fn place_perpetual_order(
        &self,
        account: [u8; 20],
        symbol: &str,
        size: U256,
        leverage: u32,
        is_long: bool,
        order_type: OrderType,
        limit_price: Option<U256>,
    ) -> Result<H256, HyperliquidError> {
        self.order_manager
            .place_order(account, symbol, size, leverage, is_long, order_type, limit_price)
            .await
    }

    pub async fn place_spot_order(
        &self,
        account: [u8; 20],
        base_token: &str,
        quote_token: &str,
        amount: U256,
        is_buy: bool,
        order_type: OrderType,
        limit_price: Option<U256>,
    ) -> Result<H256, HyperliquidError> {
        if !self.config.enable_spot_trading {
            return Err(HyperliquidError::SpotTradingDisabled);
        }

        self.order_manager
            .place_spot_order(account, base_token, quote_token, amount, is_buy, order_type, limit_price)
            .await
    }

    pub async fn get_market_data(&self, symbol: &str) -> Result<MarketData, HyperliquidError> {
        self.market_data.get_market_data(symbol).await
    }

    pub async fn get_orderbook(&self, symbol: &str, depth: u32) -> Result<OrderBook, HyperliquidError> {
        self.market_data.get_orderbook(symbol, depth).await
    }

    pub async fn get_account_info(&self, account: [u8; 20]) -> Result<AccountInfo, HyperliquidError> {
        self.connector.get_account_info(account).await
    }

    pub async fn get_positions(&self, account: [u8; 20]) -> Result<Vec<HyperliquidPosition>, HyperliquidError> {
        self.connector.get_positions(account).await
    }

    pub async fn close_position(
        &self,
        account: [u8; 20],
        position_id: H256,
        reduce_only: bool,
    ) -> Result<H256, HyperliquidError> {
        self.order_manager.close_position(account, position_id, reduce_only).await
    }

    pub async fn adjust_leverage(
        &self,
        account: [u8; 20],
        symbol: &str,
        new_leverage: u32,
    ) -> Result<(), HyperliquidError> {
        self.connector.adjust_leverage(account, symbol, new_leverage).await
    }

    pub async fn deposit_to_vault(
        &self,
        account: [u8; 20],
        vault_address: [u8; 20],
        amount: U256,
    ) -> Result<H256, HyperliquidError> {
        match &self.vault_manager {
            Some(manager) => manager.deposit(account, vault_address, amount).await,
            None => Err(HyperliquidError::VaultTradingDisabled),
        }
    }

    pub async fn withdraw_from_vault(
        &self,
        account: [u8; 20],
        vault_address: [u8; 20],
        shares: U256,
    ) -> Result<H256, HyperliquidError> {
        match &self.vault_manager {
            Some(manager) => manager.withdraw(account, vault_address, shares).await,
            None => Err(HyperliquidError::VaultTradingDisabled),
        }
    }

    pub async fn get_vault_performance(
        &self,
        vault_address: [u8; 20],
    ) -> Result<VaultPerformance, HyperliquidError> {
        match &self.vault_manager {
            Some(manager) => manager.get_performance(vault_address).await,
            None => Err(HyperliquidError::VaultTradingDisabled),
        }
    }

    pub async fn subscribe_to_market_updates(&self, symbols: Vec<String>) -> Result<(), HyperliquidError> {
        self.market_data.subscribe_to_updates(symbols).await
    }

    pub async fn get_funding_rate(&self, symbol: &str) -> Result<FundingRate, HyperliquidError> {
        self.market_data.get_funding_rate(symbol).await
    }

    pub async fn get_historical_trades(
        &self,
        symbol: &str,
        limit: u32,
    ) -> Result<Vec<Trade>, HyperliquidError> {
        self.market_data.get_historical_trades(symbol, limit).await
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OrderType {
    Market,
    Limit,
    StopLoss,
    TakeProfit,
    TrailingStop(u32), // basis points
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketData {
    pub symbol: String,
    pub index_price: U256,
    pub mark_price: U256,
    pub last_price: U256,
    pub volume_24h: U256,
    pub open_interest: U256,
    pub funding_rate: i64,
    pub next_funding_time: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderBook {
    pub symbol: String,
    pub bids: Vec<PriceLevel>,
    pub asks: Vec<PriceLevel>,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceLevel {
    pub price: U256,
    pub size: U256,
    pub num_orders: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountInfo {
    pub address: [u8; 20],
    pub balance: U256,
    pub margin_used: U256,
    pub available_margin: U256,
    pub unrealized_pnl: i128,
    pub realized_pnl: i128,
    pub total_positions: u32,
    pub account_leverage: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyperliquidPosition {
    pub id: H256,
    pub symbol: String,
    pub size: U256,
    pub entry_price: U256,
    pub mark_price: U256,
    pub liquidation_price: U256,
    pub unrealized_pnl: i128,
    pub realized_pnl: i128,
    pub margin: U256,
    pub leverage: u32,
    pub is_long: bool,
    pub funding_accrued: i128,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultPerformance {
    pub vault_address: [u8; 20],
    pub total_value_locked: U256,
    pub apy: u32, // basis points
    pub sharpe_ratio: i32,
    pub max_drawdown: u32, // basis points
    pub total_trades: u64,
    pub win_rate: u32, // percentage * 100
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FundingRate {
    pub symbol: String,
    pub rate: i64,
    pub timestamp: u64,
    pub next_funding_time: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trade {
    pub id: H256,
    pub symbol: String,
    pub price: U256,
    pub size: U256,
    pub side: TradeSide,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TradeSide {
    Buy,
    Sell,
}

#[derive(Debug)]
pub enum HyperliquidError {
    ApiError(String),
    ConnectionError,
    InvalidSymbol,
    InsufficientBalance,
    InvalidLeverage,
    OrderNotFound,
    PositionNotFound,
    SpotTradingDisabled,
    VaultTradingDisabled,
    RateLimitExceeded,
    Unauthorized,
    NetworkError,
}

impl std::error::Error for HyperliquidError {}

impl std::fmt::Display for HyperliquidError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            HyperliquidError::ApiError(msg) => write!(f, "Hyperliquid API error: {}", msg),
            HyperliquidError::ConnectionError => write!(f, "Connection error"),
            HyperliquidError::InvalidSymbol => write!(f, "Invalid symbol"),
            HyperliquidError::InsufficientBalance => write!(f, "Insufficient balance"),
            HyperliquidError::InvalidLeverage => write!(f, "Invalid leverage"),
            HyperliquidError::OrderNotFound => write!(f, "Order not found"),
            HyperliquidError::PositionNotFound => write!(f, "Position not found"),
            HyperliquidError::SpotTradingDisabled => write!(f, "Spot trading is disabled"),
            HyperliquidError::VaultTradingDisabled => write!(f, "Vault trading is disabled"),
            HyperliquidError::RateLimitExceeded => write!(f, "Rate limit exceeded"),
            HyperliquidError::Unauthorized => write!(f, "Unauthorized"),
            HyperliquidError::NetworkError => write!(f, "Network error"),
        }
    }
}

impl From<HyperliquidError> for DeFiError {
    fn from(err: HyperliquidError) -> Self {
        match err {
            HyperliquidError::InsufficientBalance => DeFiError::InsufficientBalance,
            HyperliquidError::InvalidLeverage => DeFiError::ExcessiveLeverage,
            HyperliquidError::PositionNotFound => DeFiError::PositionNotFound,
            HyperliquidError::Unauthorized => DeFiError::Unauthorized,
            _ => DeFiError::OracleError,
        }
    }
}