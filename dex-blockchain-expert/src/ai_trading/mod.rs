pub mod predictor;
pub mod optimizer;
pub mod strategy;
pub mod risk_manager;

use primitive_types::{H256, U256};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AIConfig {
    pub model_type: ModelType,
    pub prediction_horizon: u64, // seconds
    pub confidence_threshold: f64,
    pub max_position_size: U256,
    pub risk_limit: f64,
    pub rebalance_frequency: u64, // seconds
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ModelType {
    LSTM,
    Transformer,
    ReinforcementLearning,
    EnsembleModel,
}

pub struct AITradingEngine {
    config: AIConfig,
    price_predictor: Arc<predictor::PricePredictor>,
    portfolio_optimizer: Arc<optimizer::PortfolioOptimizer>,
    strategy_engine: Arc<strategy::StrategyEngine>,
    risk_manager: Arc<risk_manager::RiskManager>,
    market_data: Arc<RwLock<MarketData>>,
}

#[derive(Clone)]
struct MarketData {
    prices: Vec<PricePoint>,
    volumes: Vec<VolumePoint>,
    order_flow: Vec<OrderFlow>,
    sentiment: MarketSentiment,
}

#[derive(Clone)]
struct PricePoint {
    token: H256,
    price: U256,
    timestamp: u64,
}

#[derive(Clone)]
struct VolumePoint {
    token: H256,
    volume: U256,
    timestamp: u64,
}

#[derive(Clone)]
struct OrderFlow {
    buy_pressure: f64,
    sell_pressure: f64,
    imbalance: f64,
    timestamp: u64,
}

#[derive(Clone)]
struct MarketSentiment {
    bullish_score: f64,
    bearish_score: f64,
    neutral_score: f64,
}

impl AITradingEngine {
    pub fn new(config: AIConfig) -> Self {
        Self {
            price_predictor: Arc::new(predictor::PricePredictor::new(config.model_type.clone())),
            portfolio_optimizer: Arc::new(optimizer::PortfolioOptimizer::new()),
            strategy_engine: Arc::new(strategy::StrategyEngine::new()),
            risk_manager: Arc::new(risk_manager::RiskManager::new(config.risk_limit)),
            market_data: Arc::new(RwLock::new(MarketData {
                prices: Vec::new(),
                volumes: Vec::new(),
                order_flow: Vec::new(),
                sentiment: MarketSentiment {
                    bullish_score: 0.5,
                    bearish_score: 0.5,
                    neutral_score: 0.0,
                },
            })),
            config,
        }
    }

    pub async fn analyze_market(&self, token: H256) -> Result<MarketAnalysis, AIError> {
        let data = self.market_data.read().await;
        
        // Get price prediction
        let price_prediction = self.price_predictor.predict(
            token,
            &data.prices,
            self.config.prediction_horizon,
        ).await?;

        // Analyze order flow
        let flow_analysis = self.analyze_order_flow(&data.order_flow);
        
        // Get sentiment
        let sentiment = data.sentiment.clone();
        
        // Calculate trading signals
        let signals = self.strategy_engine.generate_signals(
            &price_prediction,
            &flow_analysis,
            &sentiment,
        ).await?;

        Ok(MarketAnalysis {
            token,
            current_price: self.get_current_price(token, &data.prices),
            predicted_price: price_prediction.predicted_price,
            confidence: price_prediction.confidence,
            trend: price_prediction.trend,
            signals,
            risk_score: self.risk_manager.calculate_risk_score(&price_prediction).await,
        })
    }

    pub async fn optimize_portfolio(
        &self,
        holdings: Vec<Holding>,
        target_return: f64,
    ) -> Result<OptimizedPortfolio, AIError> {
        let data = self.market_data.read().await;
        
        // Get predictions for all holdings
        let mut predictions = Vec::new();
        for holding in &holdings {
            let pred = self.price_predictor.predict(
                holding.token,
                &data.prices,
                self.config.prediction_horizon,
            ).await?;
            predictions.push(pred);
        }

        // Optimize allocation
        let optimized = self.portfolio_optimizer.optimize(
            holdings,
            predictions,
            target_return,
            self.config.risk_limit,
        ).await?;

        Ok(optimized)
    }

    pub async fn execute_strategy(
        &self,
        strategy_type: StrategyType,
        capital: U256,
    ) -> Result<Vec<TradeRecommendation>, AIError> {
        let data = self.market_data.read().await;
        
        // Select tokens based on strategy
        let selected_tokens = self.strategy_engine.select_tokens(
            &strategy_type,
            &data.prices,
            &data.volumes,
        ).await?;

        let mut recommendations = Vec::new();
        
        for token in selected_tokens {
            let analysis = self.analyze_market(token).await?;
            
            if analysis.confidence > self.config.confidence_threshold {
                let position_size = self.calculate_position_size(
                    capital,
                    analysis.risk_score,
                    analysis.confidence,
                );

                if let Some(signal) = analysis.signals.first() {
                    recommendations.push(TradeRecommendation {
                        token,
                        action: signal.action.clone(),
                        size: position_size,
                        entry_price: analysis.current_price,
                        target_price: analysis.predicted_price,
                        stop_loss: self.calculate_stop_loss(
                            analysis.current_price,
                            signal.action.clone(),
                            analysis.risk_score,
                        ),
                        confidence: analysis.confidence,
                        reasoning: signal.reasoning.clone(),
                    });
                }
            }
        }

        // Apply risk management
        let filtered = self.risk_manager.filter_recommendations(
            recommendations,
            capital,
        ).await?;

        Ok(filtered)
    }

    pub async fn backtest_strategy(
        &self,
        strategy: StrategyType,
        historical_data: Vec<PricePoint>,
        initial_capital: U256,
    ) -> Result<BacktestResult, AIError> {
        let results = self.strategy_engine.backtest(
            strategy,
            historical_data,
            initial_capital,
        ).await?;

        Ok(results)
    }

    pub async fn update_market_data(
        &self,
        prices: Vec<PricePoint>,
        volumes: Vec<VolumePoint>,
    ) -> Result<(), AIError> {
        let mut data = self.market_data.write().await;
        
        // Keep last 1000 data points
        data.prices.extend(prices);
        if data.prices.len() > 1000 {
            data.prices = data.prices[data.prices.len() - 1000..].to_vec();
        }
        
        data.volumes.extend(volumes);
        if data.volumes.len() > 1000 {
            data.volumes = data.volumes[data.volumes.len() - 1000..].to_vec();
        }
        
        // Update order flow
        data.order_flow = self.calculate_order_flow(&data.volumes);
        
        // Update sentiment
        data.sentiment = self.calculate_sentiment(&data.prices);
        
        Ok(())
    }

    fn analyze_order_flow(&self, flow: &[OrderFlow]) -> OrderFlowAnalysis {
        if flow.is_empty() {
            return OrderFlowAnalysis {
                trend: FlowTrend::Neutral,
                strength: 0.0,
                imbalance: 0.0,
            };
        }

        let recent_flow = &flow[flow.len().saturating_sub(10)..];
        let avg_imbalance = recent_flow.iter()
            .map(|f| f.imbalance)
            .sum::<f64>() / recent_flow.len() as f64;

        let trend = if avg_imbalance > 0.1 {
            FlowTrend::Bullish
        } else if avg_imbalance < -0.1 {
            FlowTrend::Bearish
        } else {
            FlowTrend::Neutral
        };

        OrderFlowAnalysis {
            trend,
            strength: avg_imbalance.abs(),
            imbalance: avg_imbalance,
        }
    }

    fn calculate_order_flow(&self, volumes: &[VolumePoint]) -> Vec<OrderFlow> {
        volumes.windows(2).map(|window| {
            let volume_change = window[1].volume.as_u128() as f64 / window[0].volume.as_u128().max(1) as f64 - 1.0;
            let buy_pressure = (1.0 + volume_change).max(0.0);
            let sell_pressure = (1.0 - volume_change).max(0.0);
            
            OrderFlow {
                buy_pressure,
                sell_pressure,
                imbalance: buy_pressure - sell_pressure,
                timestamp: window[1].timestamp,
            }
        }).collect()
    }

    fn calculate_sentiment(&self, prices: &[PricePoint]) -> MarketSentiment {
        if prices.len() < 2 {
            return MarketSentiment {
                bullish_score: 0.5,
                bearish_score: 0.5,
                neutral_score: 0.0,
            };
        }

        let price_changes: Vec<f64> = prices.windows(2)
            .map(|w| (w[1].price.as_u128() as f64 / w[0].price.as_u128() as f64) - 1.0)
            .collect();

        let positive = price_changes.iter().filter(|&&c| c > 0.01).count() as f64;
        let negative = price_changes.iter().filter(|&&c| c < -0.01).count() as f64;
        let neutral = price_changes.iter().filter(|&&c| c.abs() <= 0.01).count() as f64;
        let total = price_changes.len() as f64;

        MarketSentiment {
            bullish_score: positive / total,
            bearish_score: negative / total,
            neutral_score: neutral / total,
        }
    }

    fn get_current_price(&self, token: H256, prices: &[PricePoint]) -> U256 {
        prices.iter()
            .rev()
            .find(|p| p.token == token)
            .map(|p| p.price)
            .unwrap_or(U256::zero())
    }

    fn calculate_position_size(&self, capital: U256, risk_score: f64, confidence: f64) -> U256 {
        let risk_adjusted_size = capital * U256::from((confidence * 100.0) as u64) 
            / U256::from(100) * U256::from(((1.0 - risk_score) * 100.0) as u64) 
            / U256::from(100);
        
        risk_adjusted_size.min(self.config.max_position_size)
    }

    fn calculate_stop_loss(&self, entry_price: U256, action: TradeAction, risk_score: f64) -> U256 {
        let stop_percentage = U256::from((risk_score * 10.0 + 2.0) as u64); // 2-12% stop loss
        
        match action {
            TradeAction::Buy | TradeAction::Long => {
                entry_price * (U256::from(100) - stop_percentage) / U256::from(100)
            }
            TradeAction::Sell | TradeAction::Short => {
                entry_price * (U256::from(100) + stop_percentage) / U256::from(100)
            }
            _ => entry_price,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketAnalysis {
    pub token: H256,
    pub current_price: U256,
    pub predicted_price: U256,
    pub confidence: f64,
    pub trend: PriceTrend,
    pub signals: Vec<TradingSignal>,
    pub risk_score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradingSignal {
    pub action: TradeAction,
    pub strength: f64,
    pub reasoning: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TradeAction {
    Buy,
    Sell,
    Hold,
    Long,
    Short,
    ClosePosition,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PriceTrend {
    StronglyBullish,
    Bullish,
    Neutral,
    Bearish,
    StronglyBearish,
}

#[derive(Debug, Clone)]
struct OrderFlowAnalysis {
    trend: FlowTrend,
    strength: f64,
    imbalance: f64,
}

#[derive(Debug, Clone, PartialEq)]
enum FlowTrend {
    Bullish,
    Bearish,
    Neutral,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Holding {
    pub token: H256,
    pub amount: U256,
    pub average_price: U256,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptimizedPortfolio {
    pub allocations: Vec<Allocation>,
    pub expected_return: f64,
    pub risk: f64,
    pub sharpe_ratio: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Allocation {
    pub token: H256,
    pub weight: f64,
    pub amount: U256,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StrategyType {
    MomentumTrading,
    MeanReversion,
    Arbitrage,
    MarketMaking,
    TrendFollowing,
    PairTrading,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeRecommendation {
    pub token: H256,
    pub action: TradeAction,
    pub size: U256,
    pub entry_price: U256,
    pub target_price: U256,
    pub stop_loss: U256,
    pub confidence: f64,
    pub reasoning: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestResult {
    pub strategy: StrategyType,
    pub initial_capital: U256,
    pub final_capital: U256,
    pub total_return: f64,
    pub sharpe_ratio: f64,
    pub max_drawdown: f64,
    pub win_rate: f64,
    pub total_trades: usize,
}

#[derive(Debug)]
pub enum AIError {
    InsufficientData,
    ModelError(String),
    PredictionFailed,
    OptimizationFailed,
    RiskLimitExceeded,
}

impl std::error::Error for AIError {}

impl std::fmt::Display for AIError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            AIError::InsufficientData => write!(f, "Insufficient data"),
            AIError::ModelError(e) => write!(f, "Model error: {}", e),
            AIError::PredictionFailed => write!(f, "Prediction failed"),
            AIError::OptimizationFailed => write!(f, "Optimization failed"),
            AIError::RiskLimitExceeded => write!(f, "Risk limit exceeded"),
        }
    }
}