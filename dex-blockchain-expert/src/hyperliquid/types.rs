use primitive_types::{H256, U256};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyperliquidMarket {
    pub symbol: String,
    pub base_asset: String,
    pub quote_asset: String,
    pub asset_id: u32,
    pub max_leverage: u32,
    pub min_size: U256,
    pub size_increment: U256,
    pub price_precision: u32,
    pub initial_margin_fraction: f64,
    pub maintenance_margin_fraction: f64,
    pub maker_fee: i64,  // basis points, negative means rebate
    pub taker_fee: u64,  // basis points
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyperliquidOrder {
    pub id: u64,
    pub client_order_id: Option<String>,
    pub account: [u8; 20],
    pub symbol: String,
    pub side: OrderSide,
    pub order_type: HyperliquidOrderType,
    pub size: U256,
    pub price: Option<U256>,
    pub filled_size: U256,
    pub average_fill_price: Option<U256>,
    pub status: OrderStatus,
    pub reduce_only: bool,
    pub post_only: bool,
    pub time_in_force: TimeInForce,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderSide {
    Buy,
    Sell,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HyperliquidOrderType {
    Market,
    Limit,
    Stop,
    StopLimit,
    TakeProfit,
    TakeProfitLimit,
    TrailingStop,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderStatus {
    Pending,
    Open,
    PartiallyFilled,
    Filled,
    Cancelled,
    Rejected,
    Expired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimeInForce {
    GoodTillCancel,    // GTC
    ImmediateOrCancel, // IOC
    FillOrKill,        // FOK
    PostOnly,          // PO
    GoodTillTime(u64), // GTT with expiry timestamp
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyperliquidTrade {
    pub id: H256,
    pub order_id: u64,
    pub account: [u8; 20],
    pub symbol: String,
    pub side: OrderSide,
    pub price: U256,
    pub size: U256,
    pub fee: i128,  // negative means rebate
    pub fee_token: String,
    pub is_maker: bool,
    pub timestamp: u64,
    pub tx_hash: H256,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyperliquidVault {
    pub address: [u8; 20],
    pub name: String,
    pub description: String,
    pub manager: [u8; 20],
    pub total_value_locked: U256,
    pub share_price: U256,
    pub total_shares: U256,
    pub performance_fee: u32,  // basis points
    pub management_fee: u32,   // basis points annually
    pub min_deposit: U256,
    pub lock_period: u64,       // seconds
    pub strategy_type: VaultStrategy,
    pub created_at: u64,
    pub last_rebalance: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum VaultStrategy {
    MarketMaking,
    Arbitrage,
    TrendFollowing,
    DeltaNeutral,
    Yield,
    Mixed,
    Custom(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultDeposit {
    pub vault_address: [u8; 20],
    pub depositor: [u8; 20],
    pub amount: U256,
    pub shares_received: U256,
    pub share_price: U256,
    pub timestamp: u64,
    pub unlock_time: u64,
    pub tx_hash: H256,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultWithdrawal {
    pub vault_address: [u8; 20],
    pub withdrawer: [u8; 20],
    pub shares_burned: U256,
    pub amount_received: U256,
    pub share_price: U256,
    pub performance_fee_paid: U256,
    pub timestamp: u64,
    pub tx_hash: H256,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyperliquidSpotMarket {
    pub symbol: String,
    pub base_token: String,
    pub quote_token: String,
    pub base_decimals: u8,
    pub quote_decimals: u8,
    pub min_size: U256,
    pub size_increment: U256,
    pub price_precision: u32,
    pub maker_fee: i64,  // basis points
    pub taker_fee: u64,  // basis points
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossChainMessage {
    pub id: H256,
    pub source_chain: String,
    pub destination_chain: String,
    pub sender: [u8; 20],
    pub receiver: [u8; 20],
    pub message_type: MessageType,
    pub payload: Vec<u8>,
    pub nonce: u64,
    pub timestamp: u64,
    pub status: MessageStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MessageType {
    Deposit,
    Withdrawal,
    OrderPlacement,
    OrderCancellation,
    PositionSync,
    VaultAction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MessageStatus {
    Pending,
    Sent,
    Confirmed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyperliquidStats {
    pub total_volume_24h: U256,
    pub total_trades_24h: u64,
    pub total_users: u64,
    pub total_value_locked: U256,
    pub open_interest: U256,
    pub funding_rate_avg: i64,
    pub top_markets: Vec<MarketStats>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketStats {
    pub symbol: String,
    pub volume_24h: U256,
    pub trades_24h: u64,
    pub price_change_24h: i64,  // basis points
    pub high_24h: U256,
    pub low_24h: U256,
    pub open_interest: U256,
    pub funding_rate: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskMetrics {
    pub account: [u8; 20],
    pub total_exposure: U256,
    pub margin_ratio: u32,  // percentage * 100
    pub leverage: u32,
    pub unrealized_pnl: i128,
    pub realized_pnl: i128,
    pub liquidation_price: Option<U256>,
    pub health_factor: u32,  // percentage * 100
    pub var_95: U256,  // Value at Risk 95%
    pub max_drawdown: i64,  // basis points
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeeStructure {
    pub market: String,
    pub maker_fee: i64,  // basis points, negative for rebate
    pub taker_fee: u64,  // basis points
    pub vip_level: u8,
    pub volume_discount: u32,  // basis points discount
    pub token_discount: u32,   // basis points discount for using native token
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OracleData {
    pub asset: String,
    pub price: U256,
    pub confidence: u32,  // basis points
    pub timestamp: u64,
    pub sources: Vec<OracleSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OracleSource {
    pub name: String,
    pub price: U256,
    pub weight: u32,  // percentage * 100
    pub last_update: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiquidationEvent {
    pub position_id: H256,
    pub account: [u8; 20],
    pub liquidator: [u8; 20],
    pub symbol: String,
    pub size: U256,
    pub liquidation_price: U256,
    pub mark_price: U256,
    pub collateral: U256,
    pub penalty: U256,
    pub insurance_fund_contribution: U256,
    pub timestamp: u64,
    pub tx_hash: H256,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyperliquidEvent {
    pub event_type: EventType,
    pub data: EventData,
    pub timestamp: u64,
    pub block_number: u64,
    pub tx_hash: H256,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EventType {
    OrderPlaced,
    OrderCancelled,
    OrderFilled,
    PositionOpened,
    PositionClosed,
    PositionLiquidated,
    FundingPayment,
    Deposit,
    Withdrawal,
    VaultDeposit,
    VaultWithdrawal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EventData {
    Order(HyperliquidOrder),
    Trade(HyperliquidTrade),
    Position(HyperliquidPosition),
    Liquidation(LiquidationEvent),
    Transfer(TransferEvent),
    Vault(VaultEvent),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferEvent {
    pub from: [u8; 20],
    pub to: [u8; 20],
    pub token: String,
    pub amount: U256,
    pub transfer_type: TransferType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TransferType {
    Deposit,
    Withdrawal,
    Internal,
    Fee,
    Liquidation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultEvent {
    pub vault_address: [u8; 20],
    pub event_type: VaultEventType,
    pub user: [u8; 20],
    pub amount: U256,
    pub shares: U256,
    pub share_price: U256,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum VaultEventType {
    Deposit,
    Withdrawal,
    Rebalance,
    FeeCollection,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyperliquidPosition {
    pub id: H256,
    pub account: [u8; 20],
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
    pub last_funding_payment: u64,
    pub created_at: u64,
    pub updated_at: u64,
}