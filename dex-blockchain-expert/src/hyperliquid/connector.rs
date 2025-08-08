use super::{
    api::{HyperliquidApiClient, OrderRequest, CancelRequest, LeverageRequest},
    types::*,
    HyperliquidError,
    AccountInfo,
};
use primitive_types::{H256, U256};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct HyperliquidConnector {
    api_client: Arc<HyperliquidApiClient>,
    markets: Arc<RwLock<HashMap<String, HyperliquidMarket>>>,
    positions: Arc<RwLock<HashMap<[u8; 20], Vec<HyperliquidPosition>>>>,
    orders: Arc<RwLock<HashMap<[u8; 20], Vec<HyperliquidOrder>>>>,
}

impl HyperliquidConnector {
    pub async fn new(api_client: Arc<HyperliquidApiClient>) -> Result<Self, HyperliquidError> {
        let connector = Self {
            api_client,
            markets: Arc::new(RwLock::new(HashMap::new())),
            positions: Arc::new(RwLock::new(HashMap::new())),
            orders: Arc::new(RwLock::new(HashMap::new())),
        };
        
        connector.load_markets().await?;
        Ok(connector)
    }

    async fn load_markets(&self) -> Result<(), HyperliquidError> {
        let meta = self.api_client.get_meta().await?;
        let mut markets = self.markets.write().await;
        
        for asset_meta in meta.universe {
            let market = HyperliquidMarket {
                symbol: asset_meta.name.clone(),
                base_asset: asset_meta.name.clone(),
                quote_asset: "USDC".to_string(),
                asset_id: markets.len() as u32,
                max_leverage: 50,
                min_size: U256::from(1),
                size_increment: U256::from(1),
                price_precision: asset_meta.sz_decimals,
                initial_margin_fraction: 0.02,
                maintenance_margin_fraction: 0.01,
                maker_fee: -2,  // 0.02% rebate
                taker_fee: 5,    // 0.05% fee
            };
            markets.insert(asset_meta.name, market);
        }
        
        Ok(())
    }

    pub async fn get_account_info(&self, account: [u8; 20]) -> Result<AccountInfo, HyperliquidError> {
        let address = format!("0x{}", hex::encode(account));
        let user_state = self.api_client.get_user_state(&address).await?;
        
        let account_value = U256::from_dec_str(&user_state.margin_summary.account_value)
            .unwrap_or_else(|_| U256::zero());
        let margin_used = U256::from_dec_str(&user_state.margin_summary.total_margin_used)
            .unwrap_or_else(|_| U256::zero());
        
        let available_margin = if account_value > margin_used {
            account_value - margin_used
        } else {
            U256::zero()
        };
        
        let mut total_unrealized_pnl = 0i128;
        let mut total_realized_pnl = 0i128;
        
        for asset_position in &user_state.asset_positions {
            if let Ok(unrealized) = asset_position.position.unrealized_pnl.parse::<i128>() {
                total_unrealized_pnl += unrealized;
            }
            if let Ok(realized) = asset_position.position.realized_pnl.parse::<i128>() {
                total_realized_pnl += realized;
            }
        }
        
        let account_leverage = if account_value > U256::zero() {
            ((margin_used * U256::from(100)) / account_value).as_u32()
        } else {
            0
        };
        
        Ok(AccountInfo {
            address: account,
            balance: account_value,
            margin_used,
            available_margin,
            unrealized_pnl: total_unrealized_pnl,
            realized_pnl: total_realized_pnl,
            total_positions: user_state.asset_positions.len() as u32,
            account_leverage,
        })
    }

    pub async fn get_positions(&self, account: [u8; 20]) -> Result<Vec<HyperliquidPosition>, HyperliquidError> {
        let address = format!("0x{}", hex::encode(account));
        let user_state = self.api_client.get_user_state(&address).await?;
        
        let mut positions = Vec::new();
        
        for asset_position in user_state.asset_positions {
            let position = &asset_position.position;
            
            let size = U256::from_dec_str(&position.szi.replace("-", ""))
                .unwrap_or_else(|_| U256::zero());
            
            if size == U256::zero() {
                continue;
            }
            
            let is_long = !position.szi.starts_with('-');
            
            let entry_price = position.entry_px
                .as_ref()
                .and_then(|px| U256::from_dec_str(px).ok())
                .unwrap_or_else(|| U256::zero());
            
            let mark_price = U256::from_dec_str(&position.mark_px)
                .unwrap_or_else(|_| U256::zero());
            
            let unrealized_pnl = position.unrealized_pnl.parse::<i128>()
                .unwrap_or(0);
            let realized_pnl = position.realized_pnl.parse::<i128>()
                .unwrap_or(0);
            
            let funding_accrued = position.cum_funding.since_open.parse::<i128>()
                .unwrap_or(0);
            
            let leverage = position.leverage.value;
            
            let margin = if leverage > 0 {
                size * entry_price / U256::from(leverage)
            } else {
                U256::zero()
            };
            
            let liquidation_price = self.calculate_liquidation_price(
                entry_price,
                is_long,
                leverage,
            );
            
            let hyperliquid_position = HyperliquidPosition {
                id: H256::random(),
                account,
                symbol: position.coin.clone(),
                size,
                entry_price,
                mark_price,
                liquidation_price,
                unrealized_pnl,
                realized_pnl,
                margin,
                leverage,
                is_long,
                funding_accrued,
                last_funding_payment: 0,
                created_at: 0,
                updated_at: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
            };
            
            positions.push(hyperliquid_position);
        }
        
        let mut positions_cache = self.positions.write().await;
        positions_cache.insert(account, positions.clone());
        
        Ok(positions)
    }

    pub async fn adjust_leverage(
        &self,
        account: [u8; 20],
        symbol: &str,
        new_leverage: u32,
    ) -> Result<(), HyperliquidError> {
        let markets = self.markets.read().await;
        let market = markets.get(symbol)
            .ok_or(HyperliquidError::InvalidSymbol)?;
        
        if new_leverage > market.max_leverage {
            return Err(HyperliquidError::InvalidLeverage);
        }
        
        let leverage_request = LeverageRequest {
            action: super::api::LeverageAction {
                type_: "updateLeverage".to_string(),
                asset: market.asset_id,
                is_cross: true,
                leverage: new_leverage,
            },
        };
        
        self.api_client.update_leverage(leverage_request).await?;
        Ok(())
    }

    pub async fn get_open_orders(&self, account: [u8; 20]) -> Result<Vec<HyperliquidOrder>, HyperliquidError> {
        let orders_cache = self.orders.read().await;
        Ok(orders_cache.get(&account).cloned().unwrap_or_default())
    }

    pub async fn cancel_order(&self, account: [u8; 20], order_id: u64) -> Result<(), HyperliquidError> {
        let orders = self.orders.read().await;
        let user_orders = orders.get(&account)
            .ok_or(HyperliquidError::OrderNotFound)?;
        
        let order = user_orders.iter()
            .find(|o| o.id == order_id)
            .ok_or(HyperliquidError::OrderNotFound)?;
        
        let markets = self.markets.read().await;
        let market = markets.get(&order.symbol)
            .ok_or(HyperliquidError::InvalidSymbol)?;
        
        let cancel_request = CancelRequest {
            action: super::api::CancelAction {
                type_: "cancel".to_string(),
                cancels: vec![super::api::CancelDetails {
                    a: market.asset_id,
                    o: order_id,
                }],
            },
        };
        
        self.api_client.cancel_order(cancel_request).await?;
        
        drop(orders);
        let mut orders_mut = self.orders.write().await;
        if let Some(user_orders) = orders_mut.get_mut(&account) {
            user_orders.retain(|o| o.id != order_id);
        }
        
        Ok(())
    }

    pub async fn cancel_all_orders(&self, account: [u8; 20]) -> Result<(), HyperliquidError> {
        let orders = self.orders.read().await;
        let user_orders = orders.get(&account).cloned().unwrap_or_default();
        drop(orders);
        
        for order in user_orders {
            let _ = self.cancel_order(account, order.id).await;
        }
        
        Ok(())
    }

    pub async fn get_market(&self, symbol: &str) -> Result<HyperliquidMarket, HyperliquidError> {
        let markets = self.markets.read().await;
        markets.get(symbol)
            .cloned()
            .ok_or(HyperliquidError::InvalidSymbol)
    }

    pub async fn get_all_markets(&self) -> Result<Vec<HyperliquidMarket>, HyperliquidError> {
        let markets = self.markets.read().await;
        Ok(markets.values().cloned().collect())
    }

    fn calculate_liquidation_price(&self, entry_price: U256, is_long: bool, leverage: u32) -> U256 {
        if leverage == 0 {
            return U256::zero();
        }
        
        let maintenance_margin_ratio = U256::from(100);  // 1% as basis points * 100
        let leverage_u256 = U256::from(leverage);
        
        if is_long {
            let price_change = entry_price * maintenance_margin_ratio / (leverage_u256 * U256::from(10000));
            if entry_price > price_change {
                entry_price - price_change
            } else {
                U256::zero()
            }
        } else {
            let price_change = entry_price * maintenance_margin_ratio / (leverage_u256 * U256::from(10000));
            entry_price + price_change
        }
    }

    pub async fn update_position_cache(&self, account: [u8; 20], position: HyperliquidPosition) {
        let mut positions = self.positions.write().await;
        let user_positions = positions.entry(account).or_insert_with(Vec::new);
        
        if let Some(existing) = user_positions.iter_mut().find(|p| p.symbol == position.symbol) {
            *existing = position;
        } else {
            user_positions.push(position);
        }
    }

    pub async fn remove_position_from_cache(&self, account: [u8; 20], symbol: &str) {
        let mut positions = self.positions.write().await;
        if let Some(user_positions) = positions.get_mut(&account) {
            user_positions.retain(|p| p.symbol != symbol);
        }
    }

    pub async fn update_order_cache(&self, account: [u8; 20], order: HyperliquidOrder) {
        let mut orders = self.orders.write().await;
        let user_orders = orders.entry(account).or_insert_with(Vec::new);
        
        if let Some(existing) = user_orders.iter_mut().find(|o| o.id == order.id) {
            *existing = order;
        } else {
            user_orders.push(order);
        }
    }

    pub async fn get_account_risk_metrics(&self, account: [u8; 20]) -> Result<RiskMetrics, HyperliquidError> {
        let account_info = self.get_account_info(account).await?;
        let positions = self.get_positions(account).await?;
        
        let total_exposure = positions.iter()
            .map(|p| p.size * p.mark_price)
            .fold(U256::zero(), |acc, val| acc + val);
        
        let margin_ratio = if account_info.balance > U256::zero() {
            ((account_info.margin_used * U256::from(10000)) / account_info.balance).as_u32()
        } else {
            0
        };
        
        let health_factor = if margin_ratio > 0 {
            (10000 * 100) / margin_ratio
        } else {
            10000
        };
        
        let min_liquidation_price = positions.iter()
            .filter(|p| p.is_long)
            .map(|p| p.liquidation_price)
            .min();
        
        Ok(RiskMetrics {
            account,
            total_exposure,
            margin_ratio,
            leverage: account_info.account_leverage,
            unrealized_pnl: account_info.unrealized_pnl,
            realized_pnl: account_info.realized_pnl,
            liquidation_price: min_liquidation_price,
            health_factor,
            var_95: total_exposure * U256::from(200) / U256::from(10000),  // Simplified VaR
            max_drawdown: 0,
        })
    }
}