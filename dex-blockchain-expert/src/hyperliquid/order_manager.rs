use super::{
    connector::HyperliquidConnector,
    types::*,
    api::{HyperliquidApiClient, OrderRequest, OrderAction, OrderDetails, Order as ApiOrder},
    HyperliquidError,
    OrderType,
};
use primitive_types::{H256, U256};
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct OrderManager {
    connector: Arc<HyperliquidConnector>,
    pending_orders: Arc<RwLock<Vec<PendingOrder>>>,
}

#[derive(Clone)]
struct PendingOrder {
    account: [u8; 20],
    order_id: u64,
    symbol: String,
    submitted_at: u64,
}

impl OrderManager {
    pub fn new(connector: Arc<HyperliquidConnector>) -> Self {
        Self {
            connector,
            pending_orders: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub async fn place_order(
        &self,
        account: [u8; 20],
        symbol: &str,
        size: U256,
        leverage: u32,
        is_long: bool,
        order_type: OrderType,
        limit_price: Option<U256>,
    ) -> Result<H256, HyperliquidError> {
        let market = self.connector.get_market(symbol).await?;
        
        if leverage > market.max_leverage {
            return Err(HyperliquidError::InvalidLeverage);
        }
        
        if size < market.min_size {
            return Err(HyperliquidError::ApiError("Order size below minimum".to_string()));
        }
        
        let account_info = self.connector.get_account_info(account).await?;
        let required_margin = (size * limit_price.unwrap_or(U256::from(1))) / U256::from(leverage);
        
        if required_margin > account_info.available_margin {
            return Err(HyperliquidError::InsufficientBalance);
        }
        
        let order_id = self.generate_order_id();
        let tx_hash = self.submit_order_to_chain(
            account,
            symbol,
            size,
            is_long,
            order_type,
            limit_price,
        ).await?;
        
        let order = HyperliquidOrder {
            id: order_id,
            client_order_id: Some(format!("order_{}", order_id)),
            account,
            symbol: symbol.to_string(),
            side: if is_long { OrderSide::Buy } else { OrderSide::Sell },
            order_type: self.convert_order_type(&order_type),
            size,
            price: limit_price,
            filled_size: U256::zero(),
            average_fill_price: None,
            status: OrderStatus::Pending,
            reduce_only: false,
            post_only: matches!(order_type, OrderType::Limit),
            time_in_force: TimeInForce::GoodTillCancel,
            created_at: self.get_timestamp(),
            updated_at: self.get_timestamp(),
        };
        
        self.connector.update_order_cache(account, order).await;
        
        let mut pending = self.pending_orders.write().await;
        pending.push(PendingOrder {
            account,
            order_id,
            symbol: symbol.to_string(),
            submitted_at: self.get_timestamp(),
        });
        
        Ok(tx_hash)
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
        let symbol = format!("{}-{}", base_token, quote_token);
        
        let account_info = self.connector.get_account_info(account).await?;
        let required_amount = if is_buy {
            amount * limit_price.unwrap_or(U256::from(1))
        } else {
            amount
        };
        
        if required_amount > account_info.balance {
            return Err(HyperliquidError::InsufficientBalance);
        }
        
        let order_id = self.generate_order_id();
        let tx_hash = self.submit_spot_order_to_chain(
            account,
            &symbol,
            amount,
            is_buy,
            order_type,
            limit_price,
        ).await?;
        
        let order = HyperliquidOrder {
            id: order_id,
            client_order_id: Some(format!("spot_order_{}", order_id)),
            account,
            symbol,
            side: if is_buy { OrderSide::Buy } else { OrderSide::Sell },
            order_type: self.convert_order_type(&order_type),
            size: amount,
            price: limit_price,
            filled_size: U256::zero(),
            average_fill_price: None,
            status: OrderStatus::Pending,
            reduce_only: false,
            post_only: false,
            time_in_force: TimeInForce::GoodTillCancel,
            created_at: self.get_timestamp(),
            updated_at: self.get_timestamp(),
        };
        
        self.connector.update_order_cache(account, order).await;
        
        Ok(tx_hash)
    }

    pub async fn close_position(
        &self,
        account: [u8; 20],
        position_id: H256,
        reduce_only: bool,
    ) -> Result<H256, HyperliquidError> {
        let positions = self.connector.get_positions(account).await?;
        let position = positions.iter()
            .find(|p| p.id == position_id)
            .ok_or(HyperliquidError::PositionNotFound)?;
        
        let order_type = OrderType::Market;
        let is_long = !position.is_long;  // Opposite side to close
        
        let order_id = self.generate_order_id();
        let tx_hash = self.submit_order_to_chain(
            account,
            &position.symbol,
            position.size,
            is_long,
            order_type,
            None,
        ).await?;
        
        let order = HyperliquidOrder {
            id: order_id,
            client_order_id: Some(format!("close_{}", position_id)),
            account,
            symbol: position.symbol.clone(),
            side: if is_long { OrderSide::Buy } else { OrderSide::Sell },
            order_type: HyperliquidOrderType::Market,
            size: position.size,
            price: None,
            filled_size: U256::zero(),
            average_fill_price: None,
            status: OrderStatus::Pending,
            reduce_only,
            post_only: false,
            time_in_force: TimeInForce::ImmediateOrCancel,
            created_at: self.get_timestamp(),
            updated_at: self.get_timestamp(),
        };
        
        self.connector.update_order_cache(account, order).await;
        
        Ok(tx_hash)
    }

    pub async fn modify_order(
        &self,
        account: [u8; 20],
        order_id: u64,
        new_size: Option<U256>,
        new_price: Option<U256>,
    ) -> Result<(), HyperliquidError> {
        let orders = self.connector.get_open_orders(account).await?;
        let order = orders.iter()
            .find(|o| o.id == order_id)
            .ok_or(HyperliquidError::OrderNotFound)?;
        
        self.connector.cancel_order(account, order_id).await?;
        
        let size = new_size.unwrap_or(order.size);
        let price = new_price.or(order.price);
        
        self.place_order(
            account,
            &order.symbol,
            size,
            1,  // Default leverage
            order.side == OrderSide::Buy,
            OrderType::Limit,
            price,
        ).await?;
        
        Ok(())
    }

    pub async fn update_pending_orders(&self) {
        let mut pending = self.pending_orders.write().await;
        let current_time = self.get_timestamp();
        
        pending.retain(|order| {
            current_time - order.submitted_at < 60  // Keep orders pending for 60 seconds
        });
    }

    async fn submit_order_to_chain(
        &self,
        _account: [u8; 20],
        _symbol: &str,
        _size: U256,
        _is_long: bool,
        _order_type: OrderType,
        _limit_price: Option<U256>,
    ) -> Result<H256, HyperliquidError> {
        Ok(H256::random())
    }

    async fn submit_spot_order_to_chain(
        &self,
        _account: [u8; 20],
        _symbol: &str,
        _amount: U256,
        _is_buy: bool,
        _order_type: OrderType,
        _limit_price: Option<U256>,
    ) -> Result<H256, HyperliquidError> {
        Ok(H256::random())
    }

    fn convert_order_type(&self, order_type: &OrderType) -> HyperliquidOrderType {
        match order_type {
            OrderType::Market => HyperliquidOrderType::Market,
            OrderType::Limit => HyperliquidOrderType::Limit,
            OrderType::StopLoss => HyperliquidOrderType::Stop,
            OrderType::TakeProfit => HyperliquidOrderType::TakeProfit,
            OrderType::TrailingStop(_) => HyperliquidOrderType::TrailingStop,
        }
    }

    fn generate_order_id(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64
    }

    fn get_timestamp(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }
}