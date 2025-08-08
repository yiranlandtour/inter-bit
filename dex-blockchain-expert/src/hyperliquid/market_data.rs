use super::{
    api::{HyperliquidApiClient, WebSocketClient, Subscription, SubscriptionDetails},
    types::*,
    HyperliquidError,
    MarketData, OrderBook, PriceLevel, FundingRate, Trade, TradeSide,
};
use primitive_types::{H256, U256};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;

pub struct MarketDataProvider {
    api_client: Arc<HyperliquidApiClient>,
    market_data_cache: Arc<RwLock<HashMap<String, MarketData>>>,
    orderbook_cache: Arc<RwLock<HashMap<String, OrderBook>>>,
    funding_rates: Arc<RwLock<HashMap<String, FundingRate>>>,
    websocket_handle: Option<JoinHandle<()>>,
}

impl MarketDataProvider {
    pub async fn new(api_client: Arc<HyperliquidApiClient>) -> Result<Self, HyperliquidError> {
        let provider = Self {
            api_client,
            market_data_cache: Arc::new(RwLock::new(HashMap::new())),
            orderbook_cache: Arc::new(RwLock::new(HashMap::new())),
            funding_rates: Arc::new(RwLock::new(HashMap::new())),
            websocket_handle: None,
        };
        
        provider.initialize_market_data().await?;
        Ok(provider)
    }

    async fn initialize_market_data(&self) -> Result<(), HyperliquidError> {
        let all_mids = self.api_client.get_all_mids().await?;
        let mut market_data = self.market_data_cache.write().await;
        
        for (symbol, mid_price) in all_mids.mids {
            let data = MarketData {
                symbol: symbol.clone(),
                index_price: U256::from_dec_str(&mid_price).unwrap_or_else(|_| U256::zero()),
                mark_price: U256::from_dec_str(&mid_price).unwrap_or_else(|_| U256::zero()),
                last_price: U256::from_dec_str(&mid_price).unwrap_or_else(|_| U256::zero()),
                volume_24h: U256::zero(),
                open_interest: U256::zero(),
                funding_rate: 0,
                next_funding_time: self.get_next_funding_time(),
            };
            market_data.insert(symbol, data);
        }
        
        Ok(())
    }

    pub async fn get_market_data(&self, symbol: &str) -> Result<MarketData, HyperliquidError> {
        let cache = self.market_data_cache.read().await;
        if let Some(data) = cache.get(symbol) {
            return Ok(data.clone());
        }
        drop(cache);
        
        self.fetch_and_update_market_data(symbol).await
    }

    async fn fetch_and_update_market_data(&self, symbol: &str) -> Result<MarketData, HyperliquidError> {
        let all_mids = self.api_client.get_all_mids().await?;
        
        let mid_price = all_mids.mids.get(symbol)
            .ok_or(HyperliquidError::InvalidSymbol)?;
        
        let price = U256::from_dec_str(mid_price).unwrap_or_else(|_| U256::zero());
        
        let data = MarketData {
            symbol: symbol.to_string(),
            index_price: price,
            mark_price: price,
            last_price: price,
            volume_24h: U256::zero(),
            open_interest: U256::zero(),
            funding_rate: 0,
            next_funding_time: self.get_next_funding_time(),
        };\n        \n        let mut cache = self.market_data_cache.write().await;\n        cache.insert(symbol.to_string(), data.clone());\n        \n        Ok(data)\n    }\n\n    pub async fn get_orderbook(&self, symbol: &str, depth: u32) -> Result<OrderBook, HyperliquidError> {\n        let orderbook_response = self.api_client.get_orderbook(symbol).await?;\n        \n        let mut bids = Vec::new();\n        let mut asks = Vec::new();\n        \n        if orderbook_response.levels.len() > 0 {\n            for bid in &orderbook_response.levels[0] {\n                if bids.len() >= depth as usize {\n                    break;\n                }\n                bids.push(PriceLevel {\n                    price: U256::from_dec_str(&bid.px).unwrap_or_else(|_| U256::zero()),\n                    size: U256::from_dec_str(&bid.sz).unwrap_or_else(|_| U256::zero()),\n                    num_orders: bid.n,\n                });\n            }\n        }\n        \n        if orderbook_response.levels.len() > 1 {\n            for ask in &orderbook_response.levels[1] {\n                if asks.len() >= depth as usize {\n                    break;\n                }\n                asks.push(PriceLevel {\n                    price: U256::from_dec_str(&ask.px).unwrap_or_else(|_| U256::zero()),\n                    size: U256::from_dec_str(&ask.sz).unwrap_or_else(|_| U256::zero()),\n                    num_orders: ask.n,\n                });\n            }\n        }\n        \n        let orderbook = OrderBook {\n            symbol: symbol.to_string(),\n            bids,\n            asks,\n            timestamp: orderbook_response.time,\n        };\n        \n        let mut cache = self.orderbook_cache.write().await;\n        cache.insert(symbol.to_string(), orderbook.clone());\n        \n        Ok(orderbook)\n    }\n\n    pub async fn get_funding_rate(&self, symbol: &str) -> Result<FundingRate, HyperliquidError> {\n        let cache = self.funding_rates.read().await;\n        if let Some(rate) = cache.get(symbol) {\n            return Ok(rate.clone());\n        }\n        drop(cache);\n        \n        let current_time = self.get_timestamp();\n        let start_time = current_time - 86400;  // Last 24 hours\n        \n        let funding_history = self.api_client\n            .get_funding_history(symbol, start_time, current_time)\n            .await?;\n        \n        if let Some(latest) = funding_history.last() {\n            let funding_rate = FundingRate {\n                symbol: symbol.to_string(),\n                rate: latest.funding_rate.parse::<i64>().unwrap_or(0),\n                timestamp: latest.time,\n                next_funding_time: self.get_next_funding_time(),\n            };\n            \n            let mut cache = self.funding_rates.write().await;\n            cache.insert(symbol.to_string(), funding_rate.clone());\n            \n            Ok(funding_rate)\n        } else {\n            Ok(FundingRate {\n                symbol: symbol.to_string(),\n                rate: 0,\n                timestamp: current_time,\n                next_funding_time: self.get_next_funding_time(),\n            })\n        }\n    }\n\n    pub async fn get_historical_trades(&self, symbol: &str, limit: u32) -> Result<Vec<Trade>, HyperliquidError> {\n        let trade_responses = self.api_client.get_trades(symbol, limit).await?;\n        \n        let trades: Vec<Trade> = trade_responses.iter().map(|t| Trade {\n            id: H256::from_slice(t.hash.as_bytes()),\n            symbol: t.coin.clone(),\n            price: U256::from_dec_str(&t.px).unwrap_or_else(|_| U256::zero()),\n            size: U256::from_dec_str(&t.sz).unwrap_or_else(|_| U256::zero()),\n            side: if t.side == \"B\" { TradeSide::Buy } else { TradeSide::Sell },\n            timestamp: t.time,\n        }).collect();\n        \n        Ok(trades)\n    }\n\n    pub async fn subscribe_to_updates(&self, symbols: Vec<String>) -> Result<(), HyperliquidError> {\n        let api_client = self.api_client.clone();\n        let market_data_cache = self.market_data_cache.clone();\n        let orderbook_cache = self.orderbook_cache.clone();\n        \n        let handle = tokio::spawn(async move {\n            if let Ok(mut ws_client) = api_client.connect_websocket().await {\n                for symbol in symbols {\n                    let subscription = Subscription {\n                        method: \"subscribe\".to_string(),\n                        subscription: SubscriptionDetails {\n                            type_: \"trades\".to_string(),\n                            coin: Some(symbol.clone()),\n                            user: None,\n                        },\n                    };\n                    \n                    let _ = ws_client.subscribe(subscription).await;\n                    \n                    let orderbook_sub = Subscription {\n                        method: \"subscribe\".to_string(),\n                        subscription: SubscriptionDetails {\n                            type_: \"l2Book\".to_string(),\n                            coin: Some(symbol),\n                            user: None,\n                        },\n                    };\n                    \n                    let _ = ws_client.subscribe(orderbook_sub).await;\n                }\n                \n                loop {\n                    match ws_client.receive().await {\n                        Ok(message) => {\n                            // Process WebSocket messages and update caches\n                            // This is simplified - actual implementation would parse the message\n                            // and update the appropriate cache\n                        },\n                        Err(_) => break,\n                    }\n                }\n            }\n        });\n        \n        Ok(())\n    }\n\n    pub async fn get_market_stats(&self, symbol: &str) -> Result<MarketStats, HyperliquidError> {\n        let market_data = self.get_market_data(symbol).await?;\n        let trades = self.get_historical_trades(symbol, 100).await?;\n        \n        let volume_24h = trades.iter()\n            .map(|t| t.size * t.price)\n            .fold(U256::zero(), |acc, val| acc + val);\n        \n        let high_24h = trades.iter()\n            .map(|t| t.price)\n            .max()\n            .unwrap_or_else(|| U256::zero());\n        \n        let low_24h = trades.iter()\n            .map(|t| t.price)\n            .min()\n            .unwrap_or_else(|| U256::zero());\n        \n        let price_change_24h = if low_24h > U256::zero() {\n            let change = if market_data.last_price > low_24h {\n                ((market_data.last_price - low_24h) * U256::from(10000)) / low_24h\n            } else {\n                U256::zero()\n            };\n            change.as_u64() as i64\n        } else {\n            0\n        };\n        \n        Ok(MarketStats {\n            symbol: symbol.to_string(),\n            volume_24h,\n            trades_24h: trades.len() as u64,\n            price_change_24h,\n            high_24h,\n            low_24h,\n            open_interest: market_data.open_interest,\n            funding_rate: market_data.funding_rate,\n        })\n    }\n\n    pub async fn get_all_market_stats(&self) -> Result<Vec<MarketStats>, HyperliquidError> {\n        let market_data = self.market_data_cache.read().await;\n        let mut stats = Vec::new();\n        \n        for symbol in market_data.keys() {\n            if let Ok(market_stats) = self.get_market_stats(symbol).await {\n                stats.push(market_stats);\n            }\n        }\n        \n        stats.sort_by(|a, b| b.volume_24h.cmp(&a.volume_24h));\n        stats.truncate(10);  // Top 10 markets by volume\n        \n        Ok(stats)\n    }\n\n    fn get_next_funding_time(&self) -> u64 {\n        let current_time = self.get_timestamp();\n        let hours_since_epoch = current_time / 3600;\n        let next_funding_hour = ((hours_since_epoch / 8) + 1) * 8;\n        next_funding_hour * 3600\n    }\n\n    fn get_timestamp(&self) -> u64 {\n        std::time::SystemTime::now()\n            .duration_since(std::time::UNIX_EPOCH)\n            .unwrap()\n            .as_secs()\n    }\n}\n\nimpl Drop for MarketDataProvider {\n    fn drop(&mut self) {\n        if let Some(handle) = self.websocket_handle.take() {\n            handle.abort();\n        }\n    }\n}