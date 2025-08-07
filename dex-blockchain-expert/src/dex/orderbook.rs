// 高性能订单簿实现
// 支持限价单、市价单、冰山订单等多种订单类型

use super::{Order, OrderSide, OrderType, Trade, DexError};
use primitive_types::{H256, U256};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::RwLock;
use rand::Rng;

/// 订单簿
pub struct OrderBook {
    // 买单簿 (价格从高到低)
    buy_orders: Arc<RwLock<BTreeMap<U256, VecDeque<Order>>>>,
    // 卖单簿 (价格从低到高)
    sell_orders: Arc<RwLock<BTreeMap<U256, VecDeque<Order>>>>,
    // 订单索引
    order_index: Arc<RwLock<HashMap<H256, Order>>>,
    // 最新成交价
    last_price: Arc<RwLock<U256>>,
    // 24小时统计
    stats: Arc<RwLock<MarketStats>>,
}

#[derive(Debug, Clone, Default)]
struct MarketStats {
    volume_24h: U256,
    high_24h: U256,
    low_24h: U256,
    trades_24h: u64,
}

impl OrderBook {
    pub fn new() -> Self {
        Self {
            buy_orders: Arc::new(RwLock::new(BTreeMap::new())),
            sell_orders: Arc::new(RwLock::new(BTreeMap::new())),
            order_index: Arc::new(RwLock::new(HashMap::new())),
            last_price: Arc::new(RwLock::new(U256::zero())),
            stats: Arc::new(RwLock::new(MarketStats::default())),
        }
    }

    /// 添加订单到订单簿
    pub async fn add_order(&self, mut order: Order) -> Result<H256, DexError> {
        // 生成订单ID
        let order_id = self.generate_order_id(&order);
        order.id = Some(order_id);

        // 根据订单类型处理
        match order.order_type {
            OrderType::Limit => self.add_limit_order(order).await?,
            OrderType::Market => return Err(DexError::InvalidPrice), // 市价单不加入订单簿
            OrderType::StopLoss | OrderType::TakeProfit => {
                // TODO: 实现条件单逻辑
                self.add_conditional_order(order).await?;
            }
            OrderType::IcebergOrder { visible_amount } => {
                self.add_iceberg_order(order, visible_amount).await?;
            }
        }

        Ok(order_id)
    }

    /// 添加限价单
    async fn add_limit_order(&self, order: Order) -> Result<(), DexError> {
        let price = order.price;
        let order_id = order.id.unwrap();

        // 添加到索引
        self.order_index.write().await.insert(order_id, order.clone());

        // 根据买卖方向添加到对应的订单簿
        match order.side {
            OrderSide::Buy => {
                let mut buy_orders = self.buy_orders.write().await;
                buy_orders
                    .entry(price)
                    .or_insert_with(VecDeque::new)
                    .push_back(order);
            }
            OrderSide::Sell => {
                let mut sell_orders = self.sell_orders.write().await;
                sell_orders
                    .entry(price)
                    .or_insert_with(VecDeque::new)
                    .push_back(order);
            }
        }

        Ok(())
    }

    /// 添加冰山订单
    async fn add_iceberg_order(&self, mut order: Order, visible_amount: U256) -> Result<(), DexError> {
        // 将订单拆分为可见部分和隐藏部分
        let hidden_amount = order.amount - visible_amount;
        
        // 先添加可见部分
        let visible_order = Order {
            amount: visible_amount,
            ..order.clone()
        };
        self.add_limit_order(visible_order).await?;

        // 存储隐藏部分 (实际实现中应该有专门的隐藏订单管理)
        // TODO: 实现隐藏订单逻辑

        Ok(())
    }

    /// 添加条件单
    async fn add_conditional_order(&self, _order: Order) -> Result<(), DexError> {
        // TODO: 实现止损单和止盈单逻辑
        Ok(())
    }

    /// 撮合订单
    pub async fn match_order(&self, taker_order: &Order) -> Result<Vec<Trade>, DexError> {
        let mut trades = Vec::new();
        let mut remaining_amount = taker_order.amount;

        match taker_order.side {
            OrderSide::Buy => {
                // 买单与卖单簿匹配
                trades = self.match_buy_order(taker_order, remaining_amount).await?;
            }
            OrderSide::Sell => {
                // 卖单与买单簿匹配
                trades = self.match_sell_order(taker_order, remaining_amount).await?;
            }
        }

        // 更新统计信息
        self.update_stats(&trades).await;

        Ok(trades)
    }

    /// 匹配买单
    async fn match_buy_order(&self, taker_order: &Order, mut remaining: U256) -> Result<Vec<Trade>, DexError> {
        let mut trades = Vec::new();
        let mut sell_orders = self.sell_orders.write().await;

        // 从最低价开始匹配
        let prices: Vec<U256> = sell_orders.keys().cloned().collect();
        
        for price in prices {
            if remaining == U256::zero() {
                break;
            }

            // 限价单检查价格
            if taker_order.order_type == OrderType::Limit && price > taker_order.price {
                break;
            }

            if let Some(orders) = sell_orders.get_mut(&price) {
                while !orders.is_empty() && remaining > U256::zero() {
                    let maker_order = orders.front_mut().unwrap();
                    
                    let trade_amount = remaining.min(maker_order.amount);
                    
                    // 创建成交记录
                    trades.push(Trade {
                        maker_order_id: maker_order.id.unwrap(),
                        taker_order_id: taker_order.id.unwrap_or_default(),
                        amount: trade_amount,
                        price,
                        timestamp: self.current_timestamp(),
                    });

                    remaining -= trade_amount;
                    maker_order.amount -= trade_amount;

                    // 如果maker订单完全成交，移除它
                    if maker_order.amount == U256::zero() {
                        let order = orders.pop_front().unwrap();
                        self.order_index.write().await.remove(&order.id.unwrap());
                    }
                }

                // 如果该价格级别没有订单了，移除它
                if orders.is_empty() {
                    sell_orders.remove(&price);
                }
            }
        }

        Ok(trades)
    }

    /// 匹配卖单
    async fn match_sell_order(&self, taker_order: &Order, mut remaining: U256) -> Result<Vec<Trade>, DexError> {
        let mut trades = Vec::new();
        let mut buy_orders = self.buy_orders.write().await;

        // 从最高价开始匹配
        let prices: Vec<U256> = buy_orders.keys().rev().cloned().collect();
        
        for price in prices {
            if remaining == U256::zero() {
                break;
            }

            // 限价单检查价格
            if taker_order.order_type == OrderType::Limit && price < taker_order.price {
                break;
            }

            if let Some(orders) = buy_orders.get_mut(&price) {
                while !orders.is_empty() && remaining > U256::zero() {
                    let maker_order = orders.front_mut().unwrap();
                    
                    let trade_amount = remaining.min(maker_order.amount);
                    
                    trades.push(Trade {
                        maker_order_id: maker_order.id.unwrap(),
                        taker_order_id: taker_order.id.unwrap_or_default(),
                        amount: trade_amount,
                        price,
                        timestamp: self.current_timestamp(),
                    });

                    remaining -= trade_amount;
                    maker_order.amount -= trade_amount;

                    if maker_order.amount == U256::zero() {
                        let order = orders.pop_front().unwrap();
                        self.order_index.write().await.remove(&order.id.unwrap());
                    }
                }

                if orders.is_empty() {
                    buy_orders.remove(&price);
                }
            }
        }

        Ok(trades)
    }

    /// 取消订单
    pub async fn cancel_order(&self, order_id: H256) -> Result<(), DexError> {
        let mut order_index = self.order_index.write().await;
        
        let order = order_index.remove(&order_id).ok_or(DexError::OrderNotFound)?;

        // 从订单簿中移除
        match order.side {
            OrderSide::Buy => {
                let mut buy_orders = self.buy_orders.write().await;
                if let Some(orders) = buy_orders.get_mut(&order.price) {
                    orders.retain(|o| o.id != Some(order_id));
                    if orders.is_empty() {
                        buy_orders.remove(&order.price);
                    }
                }
            }
            OrderSide::Sell => {
                let mut sell_orders = self.sell_orders.write().await;
                if let Some(orders) = sell_orders.get_mut(&order.price) {
                    orders.retain(|o| o.id != Some(order_id));
                    if orders.is_empty() {
                        sell_orders.remove(&order.price);
                    }
                }
            }
        }

        Ok(())
    }

    /// 获取订单簿深度
    pub async fn get_depth(&self, levels: usize) -> OrderBookDepth {
        let buy_orders = self.buy_orders.read().await;
        let sell_orders = self.sell_orders.read().await;

        let mut bids = Vec::new();
        let mut asks = Vec::new();

        // 获取买单深度
        for (price, orders) in buy_orders.iter().rev().take(levels) {
            let total_amount: U256 = orders.iter().map(|o| o.amount).fold(U256::zero(), |acc, x| acc + x);
            bids.push(PriceLevel {
                price: *price,
                amount: total_amount,
                orders: orders.len(),
            });
        }

        // 获取卖单深度
        for (price, orders) in sell_orders.iter().take(levels) {
            let total_amount: U256 = orders.iter().map(|o| o.amount).fold(U256::zero(), |acc, x| acc + x);
            asks.push(PriceLevel {
                price: *price,
                amount: total_amount,
                orders: orders.len(),
            });
        }

        OrderBookDepth {
            bids,
            asks,
            last_price: *self.last_price.read().await,
            timestamp: self.current_timestamp(),
        }
    }

    /// 计算市价买入
    pub async fn calculate_market_buy(&self, _pair: H256, amount: U256) -> Result<U256, DexError> {
        let sell_orders = self.sell_orders.read().await;
        let mut total_cost = U256::zero();
        let mut remaining = amount;

        for (price, orders) in sell_orders.iter() {
            for order in orders {
                let trade_amount = remaining.min(order.amount);
                total_cost += trade_amount * *price;
                remaining -= trade_amount;

                if remaining == U256::zero() {
                    return Ok(total_cost);
                }
            }
        }

        if remaining > U256::zero() {
            return Err(DexError::InsufficientLiquidity);
        }

        Ok(total_cost)
    }

    /// 获取最佳买价
    pub async fn get_best_bid(&self) -> Option<U256> {
        let buy_orders = self.buy_orders.read().await;
        buy_orders.keys().rev().next().cloned()
    }

    /// 获取最佳卖价
    pub async fn get_best_ask(&self) -> Option<U256> {
        let sell_orders = self.sell_orders.read().await;
        sell_orders.keys().next().cloned()
    }

    /// 更新统计信息
    async fn update_stats(&self, trades: &[Trade]) {
        if trades.is_empty() {
            return;
        }

        let mut stats = self.stats.write().await;
        let mut last_price = self.last_price.write().await;

        for trade in trades {
            stats.volume_24h += trade.amount;
            stats.trades_24h += 1;

            // 更新最高价和最低价
            if stats.high_24h < trade.price {
                stats.high_24h = trade.price;
            }
            if stats.low_24h == U256::zero() || stats.low_24h > trade.price {
                stats.low_24h = trade.price;
            }

            *last_price = trade.price;
        }
    }

    fn generate_order_id(&self, order: &Order) -> H256 {
        use sha2::{Digest, Sha256};
        
        let mut hasher = Sha256::new();
        hasher.update(&order.trader);
        hasher.update(order.token_a.as_bytes());
        hasher.update(order.token_b.as_bytes());
        hasher.update(&order.timestamp.to_le_bytes());
        hasher.update(&rand::thread_rng().gen::<[u8; 8]>());
        
        H256::from_slice(&hasher.finalize())
    }

    fn current_timestamp(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }
}

/// 订单簿深度
#[derive(Debug, Clone)]
pub struct OrderBookDepth {
    pub bids: Vec<PriceLevel>,
    pub asks: Vec<PriceLevel>,
    pub last_price: U256,
    pub timestamp: u64,
}

#[derive(Debug, Clone)]
pub struct PriceLevel {
    pub price: U256,
    pub amount: U256,
    pub orders: usize,
}