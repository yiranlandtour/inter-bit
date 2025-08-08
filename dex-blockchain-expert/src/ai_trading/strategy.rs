use super::{
    AIError, BacktestResult, MarketSentiment, OrderFlowAnalysis,
    PricePoint, StrategyType, TradeAction, TradingSignal, VolumePoint,
};
use primitive_types::{H256, U256};
use std::collections::HashMap;

pub struct StrategyEngine {
    active_strategies: Vec<StrategyType>,
    signal_generators: HashMap<StrategyType, Box<dyn SignalGenerator>>,
}

impl StrategyEngine {
    pub fn new() -> Self {
        let mut signal_generators: HashMap<StrategyType, Box<dyn SignalGenerator>> = HashMap::new();
        
        signal_generators.insert(
            StrategyType::MomentumTrading,
            Box::new(MomentumStrategy::new()),
        );
        signal_generators.insert(
            StrategyType::MeanReversion,
            Box::new(MeanReversionStrategy::new()),
        );
        signal_generators.insert(
            StrategyType::Arbitrage,
            Box::new(ArbitrageStrategy::new()),
        );
        signal_generators.insert(
            StrategyType::MarketMaking,
            Box::new(MarketMakingStrategy::new()),
        );
        signal_generators.insert(
            StrategyType::TrendFollowing,
            Box::new(TrendFollowingStrategy::new()),
        );
        signal_generators.insert(
            StrategyType::PairTrading,
            Box::new(PairTradingStrategy::new()),
        );

        Self {
            active_strategies: vec![
                StrategyType::MomentumTrading,
                StrategyType::TrendFollowing,
            ],
            signal_generators,
        }
    }

    pub async fn generate_signals(
        &self,
        price_prediction: &super::predictor::PricePrediction,
        flow_analysis: &OrderFlowAnalysis,
        sentiment: &MarketSentiment,
    ) -> Result<Vec<TradingSignal>, AIError> {
        let mut all_signals = Vec::new();

        for strategy_type in &self.active_strategies {
            if let Some(generator) = self.signal_generators.get(strategy_type) {
                let signals = generator.generate(price_prediction, flow_analysis, sentiment)?;
                all_signals.extend(signals);
            }
        }

        // Sort by strength and deduplicate
        all_signals.sort_by(|a, b| b.strength.partial_cmp(&a.strength).unwrap());
        self.deduplicate_signals(all_signals)
    }

    pub async fn select_tokens(
        &self,
        strategy_type: &StrategyType,
        prices: &[PricePoint],
        volumes: &[VolumePoint],
    ) -> Result<Vec<H256>, AIError> {
        let mut token_scores: HashMap<H256, f64> = HashMap::new();

        // Score tokens based on strategy criteria
        match strategy_type {
            StrategyType::MomentumTrading => {
                for price in prices {
                    let momentum = self.calculate_momentum(&price.token, prices);
                    *token_scores.entry(price.token).or_insert(0.0) += momentum;
                }
            }
            StrategyType::MeanReversion => {
                for price in prices {
                    let deviation = self.calculate_deviation(&price.token, prices);
                    *token_scores.entry(price.token).or_insert(0.0) += deviation;
                }
            }
            StrategyType::MarketMaking => {
                for volume in volumes {
                    let liquidity_score = self.calculate_liquidity_score(&volume.token, volumes);
                    *token_scores.entry(volume.token).or_insert(0.0) += liquidity_score;
                }
            }
            _ => {
                // Default: select tokens with highest volume
                for volume in volumes {
                    *token_scores.entry(volume.token).or_insert(0.0) += 
                        volume.volume.as_u128() as f64;
                }
            }
        }

        // Select top tokens
        let mut sorted_tokens: Vec<_> = token_scores.into_iter().collect();
        sorted_tokens.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        
        Ok(sorted_tokens.into_iter()
            .take(10) // Top 10 tokens
            .map(|(token, _)| token)
            .collect())
    }

    pub async fn backtest(
        &self,
        strategy: StrategyType,
        historical_data: Vec<PricePoint>,
        initial_capital: U256,
    ) -> Result<BacktestResult, AIError> {
        if historical_data.len() < 100 {
            return Err(AIError::InsufficientData);
        }

        let mut capital = initial_capital;
        let mut position_size = U256::zero();
        let mut entry_price = U256::zero();
        let mut trades = Vec::new();
        let mut max_capital = initial_capital;
        let mut min_capital = initial_capital;

        // Simulate trading
        for window in historical_data.windows(20) {
            let current_price = window.last().unwrap().price;
            
            // Generate signal based on strategy
            let signal = self.generate_backtest_signal(&strategy, window);
            
            match signal {
                TradeAction::Buy if position_size == U256::zero() => {
                    // Open long position
                    position_size = capital / U256::from(2); // Use 50% of capital
                    entry_price = current_price;
                    capital = capital - position_size;
                    trades.push(Trade {
                        action: TradeAction::Buy,
                        price: current_price,
                        size: position_size,
                    });
                }
                TradeAction::Sell if position_size > U256::zero() => {
                    // Close long position
                    let proceeds = position_size * current_price / entry_price;
                    capital = capital + proceeds;
                    trades.push(Trade {
                        action: TradeAction::Sell,
                        price: current_price,
                        size: position_size,
                    });
                    position_size = U256::zero();
                    
                    // Track max/min for drawdown calculation
                    if capital > max_capital {
                        max_capital = capital;
                    }
                    if capital < min_capital {
                        min_capital = capital;
                    }
                }
                _ => {}
            }
        }

        // Close any remaining position
        if position_size > U256::zero() {
            let final_price = historical_data.last().unwrap().price;
            let proceeds = position_size * final_price / entry_price;
            capital = capital + proceeds;
        }

        // Calculate metrics
        let total_return = (capital.as_u128() as f64 / initial_capital.as_u128() as f64) - 1.0;
        let winning_trades = trades.windows(2)
            .filter(|w| w[0].action == TradeAction::Buy && w[1].action == TradeAction::Sell)
            .filter(|w| w[1].price > w[0].price)
            .count();
        let total_paired_trades = trades.len() / 2;
        let win_rate = if total_paired_trades > 0 {
            winning_trades as f64 / total_paired_trades as f64
        } else {
            0.0
        };

        let max_drawdown = if max_capital > U256::zero() {
            1.0 - (min_capital.as_u128() as f64 / max_capital.as_u128() as f64)
        } else {
            0.0
        };

        // Simplified Sharpe ratio calculation
        let sharpe_ratio = if total_return > 0.0 {
            total_return / max_drawdown.max(0.1)
        } else {
            0.0
        };

        Ok(BacktestResult {
            strategy,
            initial_capital,
            final_capital: capital,
            total_return,
            sharpe_ratio,
            max_drawdown,
            win_rate,
            total_trades: trades.len(),
        })
    }

    fn deduplicate_signals(&self, signals: Vec<TradingSignal>) -> Result<Vec<TradingSignal>, AIError> {
        let mut unique_signals = Vec::new();
        let mut seen_actions = std::collections::HashSet::new();

        for signal in signals {
            if seen_actions.insert(signal.action.clone()) {
                unique_signals.push(signal);
                if unique_signals.len() >= 3 {
                    break;
                }
            }
        }

        Ok(unique_signals)
    }

    fn calculate_momentum(&self, token: &H256, prices: &[PricePoint]) -> f64 {
        let token_prices: Vec<_> = prices.iter()
            .filter(|p| p.token == *token)
            .map(|p| p.price.as_u128() as f64)
            .collect();

        if token_prices.len() < 2 {
            return 0.0;
        }

        let recent = token_prices.len().saturating_sub(5);
        let recent_avg = token_prices[recent..].iter().sum::<f64>() / 5.0;
        let older_avg = token_prices[..recent].iter().sum::<f64>() / recent.max(1) as f64;

        (recent_avg - older_avg) / older_avg
    }

    fn calculate_deviation(&self, token: &H256, prices: &[PricePoint]) -> f64 {
        let token_prices: Vec<_> = prices.iter()
            .filter(|p| p.token == *token)
            .map(|p| p.price.as_u128() as f64)
            .collect();

        if token_prices.is_empty() {
            return 0.0;
        }

        let mean = token_prices.iter().sum::<f64>() / token_prices.len() as f64;
        let current = token_prices.last().unwrap_or(&mean);
        
        ((current - mean) / mean).abs()
    }

    fn calculate_liquidity_score(&self, token: &H256, volumes: &[VolumePoint]) -> f64 {
        volumes.iter()
            .filter(|v| v.token == *token)
            .map(|v| v.volume.as_u128() as f64)
            .sum::<f64>()
            / volumes.len().max(1) as f64
    }

    fn generate_backtest_signal(&self, strategy: &StrategyType, window: &[PricePoint]) -> TradeAction {
        if window.len() < 2 {
            return TradeAction::Hold;
        }

        let prices: Vec<f64> = window.iter().map(|p| p.price.as_u128() as f64).collect();
        let last_price = prices.last().unwrap();
        let prev_price = prices[prices.len() - 2];

        match strategy {
            StrategyType::MomentumTrading => {
                let momentum = (*last_price - prev_price) / prev_price;
                if momentum > 0.02 {
                    TradeAction::Buy
                } else if momentum < -0.02 {
                    TradeAction::Sell
                } else {
                    TradeAction::Hold
                }
            }
            StrategyType::MeanReversion => {
                let mean = prices.iter().sum::<f64>() / prices.len() as f64;
                if *last_price < mean * 0.95 {
                    TradeAction::Buy
                } else if *last_price > mean * 1.05 {
                    TradeAction::Sell
                } else {
                    TradeAction::Hold
                }
            }
            StrategyType::TrendFollowing => {
                let ma_short = prices[prices.len().saturating_sub(5)..].iter().sum::<f64>() / 5.0;
                let ma_long = prices.iter().sum::<f64>() / prices.len() as f64;
                
                if ma_short > ma_long * 1.01 {
                    TradeAction::Buy
                } else if ma_short < ma_long * 0.99 {
                    TradeAction::Sell
                } else {
                    TradeAction::Hold
                }
            }
            _ => TradeAction::Hold,
        }
    }
}

struct Trade {
    action: TradeAction,
    price: U256,
    size: U256,
}

trait SignalGenerator: Send + Sync {
    fn generate(
        &self,
        price_prediction: &super::predictor::PricePrediction,
        flow_analysis: &OrderFlowAnalysis,
        sentiment: &MarketSentiment,
    ) -> Result<Vec<TradingSignal>, AIError>;
}

struct MomentumStrategy {
    momentum_threshold: f64,
}

impl MomentumStrategy {
    fn new() -> Self {
        Self {
            momentum_threshold: 0.03,
        }
    }
}

impl SignalGenerator for MomentumStrategy {
    fn generate(
        &self,
        price_prediction: &super::predictor::PricePrediction,
        flow_analysis: &OrderFlowAnalysis,
        sentiment: &MarketSentiment,
    ) -> Result<Vec<TradingSignal>, AIError> {
        let mut signals = Vec::new();
        
        if price_prediction.confidence > 0.7 {
            let momentum_score = flow_analysis.strength * sentiment.bullish_score;
            
            if momentum_score > self.momentum_threshold {
                signals.push(TradingSignal {
                    action: TradeAction::Buy,
                    strength: momentum_score,
                    reasoning: format!("Strong momentum detected with {:.1}% confidence", 
                        price_prediction.confidence * 100.0),
                });
            }
        }
        
        Ok(signals)
    }
}

struct MeanReversionStrategy {
    deviation_threshold: f64,
}

impl MeanReversionStrategy {
    fn new() -> Self {
        Self {
            deviation_threshold: 0.05,
        }
    }
}

impl SignalGenerator for MeanReversionStrategy {
    fn generate(
        &self,
        price_prediction: &super::predictor::PricePrediction,
        flow_analysis: &OrderFlowAnalysis,
        sentiment: &MarketSentiment,
    ) -> Result<Vec<TradingSignal>, AIError> {
        let mut signals = Vec::new();
        
        // Check for oversold/overbought conditions
        if sentiment.bearish_score > 0.7 && flow_analysis.imbalance < -0.2 {
            signals.push(TradingSignal {
                action: TradeAction::Buy,
                strength: sentiment.bearish_score,
                reasoning: "Oversold condition detected, expecting mean reversion".to_string(),
            });
        } else if sentiment.bullish_score > 0.7 && flow_analysis.imbalance > 0.2 {
            signals.push(TradingSignal {
                action: TradeAction::Sell,
                strength: sentiment.bullish_score,
                reasoning: "Overbought condition detected, expecting mean reversion".to_string(),
            });
        }
        
        Ok(signals)
    }
}

struct ArbitrageStrategy;

impl ArbitrageStrategy {
    fn new() -> Self {
        Self
    }
}

impl SignalGenerator for ArbitrageStrategy {
    fn generate(
        &self,
        price_prediction: &super::predictor::PricePrediction,
        flow_analysis: &OrderFlowAnalysis,
        sentiment: &MarketSentiment,
    ) -> Result<Vec<TradingSignal>, AIError> {
        // Simplified arbitrage signal
        let mut signals = Vec::new();
        
        if flow_analysis.imbalance.abs() > 0.1 {
            signals.push(TradingSignal {
                action: if flow_analysis.imbalance > 0.0 {
                    TradeAction::Sell
                } else {
                    TradeAction::Buy
                },
                strength: flow_analysis.imbalance.abs(),
                reasoning: "Price imbalance detected for arbitrage opportunity".to_string(),
            });
        }
        
        Ok(signals)
    }
}

struct MarketMakingStrategy {
    spread_threshold: f64,
}

impl MarketMakingStrategy {
    fn new() -> Self {
        Self {
            spread_threshold: 0.002, // 0.2% spread
        }
    }
}

impl SignalGenerator for MarketMakingStrategy {
    fn generate(
        &self,
        price_prediction: &super::predictor::PricePrediction,
        flow_analysis: &OrderFlowAnalysis,
        sentiment: &MarketSentiment,
    ) -> Result<Vec<TradingSignal>, AIError> {
        let mut signals = Vec::new();
        
        // Place both buy and sell orders for market making
        if sentiment.neutral_score > 0.5 {
            signals.push(TradingSignal {
                action: TradeAction::Buy,
                strength: 0.5,
                reasoning: format!("Market making buy order at {:.2}% below mid", 
                    self.spread_threshold * 100.0),
            });
            signals.push(TradingSignal {
                action: TradeAction::Sell,
                strength: 0.5,
                reasoning: format!("Market making sell order at {:.2}% above mid", 
                    self.spread_threshold * 100.0),
            });
        }
        
        Ok(signals)
    }
}

struct TrendFollowingStrategy {
    trend_strength_threshold: f64,
}

impl TrendFollowingStrategy {
    fn new() -> Self {
        Self {
            trend_strength_threshold: 0.6,
        }
    }
}

impl SignalGenerator for TrendFollowingStrategy {
    fn generate(
        &self,
        price_prediction: &super::predictor::PricePrediction,
        flow_analysis: &OrderFlowAnalysis,
        sentiment: &MarketSentiment,
    ) -> Result<Vec<TradingSignal>, AIError> {
        use super::PriceTrend;
        
        let mut signals = Vec::new();
        
        match price_prediction.trend {
            PriceTrend::StronglyBullish | PriceTrend::Bullish => {
                if sentiment.bullish_score > self.trend_strength_threshold {
                    signals.push(TradingSignal {
                        action: TradeAction::Long,
                        strength: sentiment.bullish_score,
                        reasoning: "Strong bullish trend confirmed".to_string(),
                    });
                }
            }
            PriceTrend::StronglyBearish | PriceTrend::Bearish => {
                if sentiment.bearish_score > self.trend_strength_threshold {
                    signals.push(TradingSignal {
                        action: TradeAction::Short,
                        strength: sentiment.bearish_score,
                        reasoning: "Strong bearish trend confirmed".to_string(),
                    });
                }
            }
            PriceTrend::Neutral => {
                signals.push(TradingSignal {
                    action: TradeAction::Hold,
                    strength: 0.5,
                    reasoning: "No clear trend detected".to_string(),
                });
            }
        }
        
        Ok(signals)
    }
}

struct PairTradingStrategy {
    correlation_threshold: f64,
}

impl PairTradingStrategy {
    fn new() -> Self {
        Self {
            correlation_threshold: 0.8,
        }
    }
}

impl SignalGenerator for PairTradingStrategy {
    fn generate(
        &self,
        price_prediction: &super::predictor::PricePrediction,
        flow_analysis: &OrderFlowAnalysis,
        sentiment: &MarketSentiment,
    ) -> Result<Vec<TradingSignal>, AIError> {
        let mut signals = Vec::new();
        
        // Simplified pair trading signal
        if flow_analysis.imbalance.abs() > 0.05 {
            signals.push(TradingSignal {
                action: if flow_analysis.imbalance > 0.0 {
                    TradeAction::Sell  // Sell overperforming asset
                } else {
                    TradeAction::Buy   // Buy underperforming asset
                },
                strength: flow_analysis.imbalance.abs() * self.correlation_threshold,
                reasoning: "Pair divergence detected for statistical arbitrage".to_string(),
            });
        }
        
        Ok(signals)
    }
}