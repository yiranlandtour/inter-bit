use super::{HyperliquidConfig, HyperliquidError};
use primitive_types::{H256, U256};
use reqwest::{Client, Response};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_tungstenite::{connect_async, WebSocketStream};
use futures_util::{StreamExt, SinkExt};
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

pub struct HyperliquidApiClient {
    config: HyperliquidConfig,
    http_client: Client,
    rate_limiter: Arc<RwLock<RateLimiter>>,
}

impl HyperliquidApiClient {
    pub fn new(config: HyperliquidConfig) -> Result<Self, HyperliquidError> {
        let http_client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|_| HyperliquidError::NetworkError)?;

        Ok(Self {
            config,
            http_client,
            rate_limiter: Arc::new(RwLock::new(RateLimiter::new(100, 60))), // 100 requests per minute
        })
    }

    pub async fn get_meta(&self) -> Result<MetaResponse, HyperliquidError> {
        let url = format!("{}/meta", self.config.api_endpoint);
        let response = self.make_get_request(&url).await?;
        self.parse_response(response).await
    }

    pub async fn get_asset_contexts(&self) -> Result<Vec<AssetContext>, HyperliquidError> {
        let url = format!("{}/info", self.config.api_endpoint);
        let body = json!({
            "type": "meta"
        });
        
        let response = self.make_post_request(&url, body).await?;
        self.parse_response(response).await
    }

    pub async fn get_user_state(&self, address: &str) -> Result<UserState, HyperliquidError> {
        let url = format!("{}/info", self.config.api_endpoint);
        let body = json!({
            "type": "clearinghouseState",
            "user": address
        });
        
        let response = self.make_post_request(&url, body).await?;
        self.parse_response(response).await
    }

    pub async fn get_orderbook(&self, symbol: &str) -> Result<OrderbookResponse, HyperliquidError> {
        let url = format!("{}/info", self.config.api_endpoint);
        let body = json!({
            "type": "l2Book",
            "coin": symbol
        });
        
        let response = self.make_post_request(&url, body).await?;
        self.parse_response(response).await
    }

    pub async fn get_all_mids(&self) -> Result<AllMidsResponse, HyperliquidError> {
        let url = format!("{}/info", self.config.api_endpoint);
        let body = json!({
            "type": "allMids"
        });
        
        let response = self.make_post_request(&url, body).await?;
        self.parse_response(response).await
    }

    pub async fn place_order(&self, order_request: OrderRequest) -> Result<OrderResponse, HyperliquidError> {
        self.rate_limiter.write().await.check_rate_limit()?;
        
        let url = format!("{}/exchange", self.config.api_endpoint);
        let body = self.sign_request(order_request)?;
        
        let response = self.make_post_request(&url, body).await?;
        self.parse_response(response).await
    }

    pub async fn cancel_order(&self, cancel_request: CancelRequest) -> Result<CancelResponse, HyperliquidError> {
        self.rate_limiter.write().await.check_rate_limit()?;
        
        let url = format!("{}/exchange", self.config.api_endpoint);
        let body = self.sign_request(cancel_request)?;
        
        let response = self.make_post_request(&url, body).await?;
        self.parse_response(response).await
    }

    pub async fn update_leverage(&self, leverage_request: LeverageRequest) -> Result<(), HyperliquidError> {
        self.rate_limiter.write().await.check_rate_limit()?;
        
        let url = format!("{}/exchange", self.config.api_endpoint);
        let body = self.sign_request(leverage_request)?;
        
        let response = self.make_post_request(&url, body).await?;
        let _: Value = self.parse_response(response).await?;
        Ok(())
    }

    pub async fn get_trades(&self, symbol: &str, limit: u32) -> Result<Vec<TradeResponse>, HyperliquidError> {
        let url = format!("{}/info", self.config.api_endpoint);
        let body = json!({
            "type": "trades",
            "coin": symbol,
            "limit": limit
        });
        
        let response = self.make_post_request(&url, body).await?;
        self.parse_response(response).await
    }

    pub async fn get_funding_history(&self, symbol: &str, start_time: u64, end_time: u64) -> Result<Vec<FundingHistoryItem>, HyperliquidError> {
        let url = format!("{}/info", self.config.api_endpoint);
        let body = json!({
            "type": "fundingHistory",
            "coin": symbol,
            "startTime": start_time,
            "endTime": end_time
        });
        
        let response = self.make_post_request(&url, body).await?;
        self.parse_response(response).await
    }

    pub async fn connect_websocket(&self) -> Result<WebSocketClient, HyperliquidError> {
        WebSocketClient::new(&self.config.websocket_endpoint).await
    }

    async fn make_get_request(&self, url: &str) -> Result<Response, HyperliquidError> {
        self.rate_limiter.write().await.check_rate_limit()?;
        
        self.http_client
            .get(url)
            .send()
            .await
            .map_err(|_| HyperliquidError::NetworkError)
    }

    async fn make_post_request(&self, url: &str, body: Value) -> Result<Response, HyperliquidError> {
        self.rate_limiter.write().await.check_rate_limit()?;
        
        self.http_client
            .post(url)
            .json(&body)
            .send()
            .await
            .map_err(|_| HyperliquidError::NetworkError)
    }

    async fn parse_response<T: for<'de> Deserialize<'de>>(&self, response: Response) -> Result<T, HyperliquidError> {
        if !response.status().is_success() {
            let error_text = response.text().await.unwrap_or_else(|_| "Unknown error".to_string());
            return Err(HyperliquidError::ApiError(error_text));
        }

        response
            .json()
            .await
            .map_err(|_| HyperliquidError::ApiError("Failed to parse response".to_string()))
    }

    fn sign_request<T: Serialize>(&self, request: T) -> Result<Value, HyperliquidError> {
        let mut body = serde_json::to_value(request)
            .map_err(|_| HyperliquidError::ApiError("Failed to serialize request".to_string()))?;
        
        if let (Some(api_key), Some(api_secret)) = (&self.config.api_key, &self.config.api_secret) {
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64;
            
            let message = format!("{}{}", timestamp, serde_json::to_string(&body).unwrap());
            
            let mut mac = HmacSha256::new_from_slice(api_secret.as_bytes())
                .map_err(|_| HyperliquidError::ApiError("Invalid API secret".to_string()))?;
            mac.update(message.as_bytes());
            let signature = hex::encode(mac.finalize().into_bytes());
            
            if let Value::Object(ref mut map) = body {
                map.insert("signature".to_string(), json!(signature));
                map.insert("timestamp".to_string(), json!(timestamp));
            }
        }
        
        Ok(body)
    }
}

pub struct WebSocketClient {
    stream: WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
}

impl WebSocketClient {
    pub async fn new(endpoint: &str) -> Result<Self, HyperliquidError> {
        let (stream, _) = connect_async(endpoint)
            .await
            .map_err(|_| HyperliquidError::ConnectionError)?;
        
        Ok(Self { stream })
    }

    pub async fn subscribe(&mut self, subscription: Subscription) -> Result<(), HyperliquidError> {
        let message = serde_json::to_string(&subscription)
            .map_err(|_| HyperliquidError::ApiError("Failed to serialize subscription".to_string()))?;
        
        self.stream
            .send(tokio_tungstenite::tungstenite::Message::Text(message))
            .await
            .map_err(|_| HyperliquidError::ConnectionError)?;
        
        Ok(())
    }

    pub async fn receive(&mut self) -> Result<WebSocketMessage, HyperliquidError> {
        match self.stream.next().await {
            Some(Ok(msg)) => {
                if let tokio_tungstenite::tungstenite::Message::Text(text) = msg {
                    serde_json::from_str(&text)
                        .map_err(|_| HyperliquidError::ApiError("Failed to parse WebSocket message".to_string()))
                } else {
                    Err(HyperliquidError::ApiError("Unexpected message type".to_string()))
                }
            }
            _ => Err(HyperliquidError::ConnectionError),
        }
    }
}

struct RateLimiter {
    max_requests: u32,
    window_seconds: u64,
    requests: Vec<u64>,
}

impl RateLimiter {
    fn new(max_requests: u32, window_seconds: u64) -> Self {
        Self {
            max_requests,
            window_seconds,
            requests: Vec::new(),
        }
    }

    fn check_rate_limit(&mut self) -> Result<(), HyperliquidError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        
        self.requests.retain(|&timestamp| now - timestamp < self.window_seconds);
        
        if self.requests.len() >= self.max_requests as usize {
            return Err(HyperliquidError::RateLimitExceeded);
        }
        
        self.requests.push(now);
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MetaResponse {
    pub universe: Vec<AssetMeta>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AssetMeta {
    pub name: String,
    pub sz_decimals: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AssetContext {
    pub coin: String,
    pub ctx: MarketContext,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MarketContext {
    pub funding: String,
    pub open_interest: String,
    pub prev_day_px: String,
    pub day_ntl_vlm: String,
    pub premium: String,
    pub oracle_px: String,
    pub mark_px: String,
    pub mid_px: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UserState {
    pub margin_summary: MarginSummary,
    pub asset_positions: Vec<AssetPosition>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MarginSummary {
    pub account_value: String,
    pub total_margin_used: String,
    pub total_ntl_pos: String,
    pub total_raw_usd: String,
    pub total_ntl_pos_abs: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AssetPosition {
    pub position: Position,
    pub type_: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Position {
    pub coin: String,
    pub szi: String,
    pub leverage: Leverage,
    pub entry_px: Option<String>,
    pub mark_px: String,
    pub unrealized_pnl: String,
    pub realized_pnl: String,
    pub cum_funding: CumFunding,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Leverage {
    pub type_: String,
    pub value: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CumFunding {
    pub all_time: String,
    pub since_open: String,
    pub since_change: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct OrderbookResponse {
    pub coin: String,
    pub levels: Vec<Vec<Level>>,
    pub time: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Level {
    pub px: String,
    pub sz: String,
    pub n: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AllMidsResponse {
    pub mids: std::collections::HashMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct OrderRequest {
    pub action: OrderAction,
    pub order_type: OrderRequestType,
    pub orders: Vec<Order>,
    pub grouping: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct OrderAction {
    pub type_: String,
    pub orders: Vec<OrderDetails>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct OrderDetails {
    pub a: u32,    // asset
    pub b: bool,    // is_buy
    pub p: String,  // price
    pub s: String,  // size
    pub r: bool,    // reduce_only
    pub t: OrderRequestType,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Order {
    pub coin: String,
    pub is_buy: bool,
    pub sz: String,
    pub limit_px: String,
    pub order_type: OrderRequestType,
    pub reduce_only: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum OrderRequestType {
    Limit,
    Market,
    Stop,
    TakeProfit,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct OrderResponse {
    pub status: String,
    pub response: OrderResponseData,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct OrderResponseData {
    pub type_: String,
    pub data: OrderResponseDetails,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct OrderResponseDetails {
    pub statuses: Vec<OrderStatus>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct OrderStatus {
    pub resting: RestingOrder,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RestingOrder {
    pub oid: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CancelRequest {
    pub action: CancelAction,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CancelAction {
    pub type_: String,
    pub cancels: Vec<CancelDetails>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CancelDetails {
    pub a: u32,  // asset
    pub o: u64,  // order_id
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CancelResponse {
    pub status: String,
    pub response: CancelResponseData,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CancelResponseData {
    pub type_: String,
    pub data: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LeverageRequest {
    pub action: LeverageAction,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LeverageAction {
    pub type_: String,
    pub asset: u32,
    pub is_cross: bool,
    pub leverage: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TradeResponse {
    pub coin: String,
    pub side: String,
    pub px: String,
    pub sz: String,
    pub hash: String,
    pub time: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FundingHistoryItem {
    pub coin: String,
    pub funding_rate: String,
    pub premium: String,
    pub time: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Subscription {
    pub method: String,
    pub subscription: SubscriptionDetails,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SubscriptionDetails {
    pub type_: String,
    pub coin: Option<String>,
    pub user: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WebSocketMessage {
    pub channel: String,
    pub data: Value,
}