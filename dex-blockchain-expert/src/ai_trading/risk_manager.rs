use super::{AIError, TradeRecommendation};
use primitive_types::{H256, U256};
use std::collections::HashMap;

pub struct RiskManager {
    risk_limit: f64,
    var_calculator: VaRCalculator,
    exposure_tracker: ExposureTracker,
    stop_loss_manager: StopLossManager,
}

impl RiskManager {
    pub fn new(risk_limit: f64) -> Self {
        Self {
            risk_limit,
            var_calculator: VaRCalculator::new(),
            exposure_tracker: ExposureTracker::new(),
            stop_loss_manager: StopLossManager::new(),
        }
    }

    pub async fn calculate_risk_score(
        &self,
        prediction: &super::predictor::PricePrediction,
    ) -> f64 {
        // Risk score based on multiple factors
        let confidence_risk = 1.0 - prediction.confidence;
        let volatility_risk = self.estimate_volatility_risk(prediction);
        let trend_risk = self.calculate_trend_risk(&prediction.trend);
        
        // Weighted average of risk factors
        (confidence_risk * 0.4 + volatility_risk * 0.4 + trend_risk * 0.2).min(1.0)
    }

    pub async fn filter_recommendations(
        &self,
        recommendations: Vec<TradeRecommendation>,
        capital: U256,
    ) -> Result<Vec<TradeRecommendation>, AIError> {
        let mut filtered = Vec::new();
        let mut total_exposure = U256::zero();
        let mut exposure_by_token: HashMap<H256, U256> = HashMap::new();

        for mut rec in recommendations {
            // Check total exposure
            if total_exposure + rec.size > capital * U256::from(95) / U256::from(100) {
                continue; // Skip if exceeds 95% of capital
            }

            // Check per-token exposure
            let token_exposure = exposure_by_token.entry(rec.token).or_insert(U256::zero());
            if *token_exposure + rec.size > capital * U256::from(30) / U256::from(100) {
                continue; // Skip if single token exceeds 30% of capital
            }

            // Apply position sizing based on risk
            let risk_adjusted_size = self.adjust_position_size(
                rec.size,
                rec.confidence,
                capital,
            );
            rec.size = risk_adjusted_size;

            // Set appropriate stop loss
            rec.stop_loss = self.stop_loss_manager.calculate_stop_loss(
                rec.entry_price,
                rec.action.clone(),
                rec.confidence,
            );

            // Update exposure tracking
            total_exposure = total_exposure + rec.size;
            *token_exposure = *token_exposure + rec.size;

            filtered.push(rec);

            // Limit number of concurrent positions
            if filtered.len() >= 10 {
                break;
            }
        }

        Ok(filtered)
    }

    pub async fn validate_trade(
        &self,
        trade: &TradeRecommendation,
        current_portfolio: &Portfolio,
    ) -> Result<bool, AIError> {
        // Check risk limits
        if !self.check_risk_limits(trade, current_portfolio) {
            return Ok(false);
        }

        // Check correlation risk
        if !self.check_correlation_risk(trade, current_portfolio) {
            return Ok(false);
        }

        // Check liquidity risk
        if !self.check_liquidity_risk(trade) {
            return Ok(false);
        }

        Ok(true)
    }

    pub async fn monitor_positions(
        &self,
        positions: Vec<Position>,
        current_prices: HashMap<H256, U256>,
    ) -> Vec<RiskAlert> {
        let mut alerts = Vec::new();

        for position in positions {
            if let Some(&current_price) = current_prices.get(&position.token) {
                // Check stop loss
                if self.should_trigger_stop_loss(&position, current_price) {
                    alerts.push(RiskAlert {
                        token: position.token,
                        alert_type: AlertType::StopLoss,
                        severity: Severity::High,
                        message: format!("Stop loss triggered at price {}", current_price),
                        action: Some(RiskAction::ClosePosition),
                    });
                }

                // Check drawdown
                let drawdown = self.calculate_drawdown(&position, current_price);
                if drawdown > 0.1 { // 10% drawdown
                    alerts.push(RiskAlert {
                        token: position.token,
                        alert_type: AlertType::Drawdown,
                        severity: Severity::Medium,
                        message: format!("Position down {:.1}%", drawdown * 100.0),
                        action: Some(RiskAction::ReducePosition),
                    });
                }

                // Check profit target
                if self.should_take_profit(&position, current_price) {
                    alerts.push(RiskAlert {
                        token: position.token,
                        alert_type: AlertType::TakeProfit,
                        severity: Severity::Low,
                        message: format!("Profit target reached at price {}", current_price),
                        action: Some(RiskAction::ClosePosition),
                    });
                }
            }
        }

        alerts
    }

    fn estimate_volatility_risk(&self, prediction: &super::predictor::PricePrediction) -> f64 {
        use super::PriceTrend;
        
        match prediction.trend {
            PriceTrend::StronglyBullish | PriceTrend::StronglyBearish => 0.8,
            PriceTrend::Bullish | PriceTrend::Bearish => 0.5,
            PriceTrend::Neutral => 0.3,
        }
    }

    fn calculate_trend_risk(&self, trend: &super::PriceTrend) -> f64 {
        use super::PriceTrend;
        
        match trend {
            PriceTrend::StronglyBullish | PriceTrend::StronglyBearish => 0.6,
            PriceTrend::Bullish | PriceTrend::Bearish => 0.4,
            PriceTrend::Neutral => 0.2,
        }
    }

    fn adjust_position_size(&self, size: U256, confidence: f64, capital: U256) -> U256 {
        // Kelly Criterion inspired position sizing
        let kelly_fraction = (confidence - (1.0 - confidence)) / 1.0;
        let adjusted_fraction = (kelly_fraction * 0.25).max(0.0).min(0.2); // Conservative Kelly (25% of full Kelly)
        
        let max_size = capital * U256::from((adjusted_fraction * 100.0) as u64) / U256::from(100);
        size.min(max_size)
    }

    fn check_risk_limits(&self, trade: &TradeRecommendation, portfolio: &Portfolio) -> bool {
        let portfolio_risk = self.calculate_portfolio_risk(portfolio);
        let trade_risk = self.calculate_trade_risk(trade);
        
        portfolio_risk + trade_risk <= self.risk_limit
    }

    fn check_correlation_risk(&self, trade: &TradeRecommendation, portfolio: &Portfolio) -> bool {
        // Check if adding this trade increases correlation risk
        let correlation_with_portfolio = self.calculate_correlation(trade.token, portfolio);
        correlation_with_portfolio < 0.7 // Avoid highly correlated positions
    }

    fn check_liquidity_risk(&self, trade: &TradeRecommendation) -> bool {
        // Simplified liquidity check
        trade.size < U256::from(1000000) * U256::from(10u128.pow(18)) // Max 1M tokens
    }

    fn calculate_portfolio_risk(&self, portfolio: &Portfolio) -> f64 {
        portfolio.positions.iter()
            .map(|p| (p.size.as_u128() as f64 / portfolio.total_value.as_u128() as f64) * 0.1)
            .sum()
    }

    fn calculate_trade_risk(&self, trade: &TradeRecommendation) -> f64 {
        (1.0 - trade.confidence) * 0.2
    }

    fn calculate_correlation(&self, token: H256, portfolio: &Portfolio) -> f64 {
        // Simplified correlation calculation
        if portfolio.positions.iter().any(|p| p.token == token) {
            1.0 // Same token = perfect correlation
        } else {
            0.3 // Assume low correlation for different tokens
        }
    }

    fn should_trigger_stop_loss(&self, position: &Position, current_price: U256) -> bool {
        match position.direction {
            Direction::Long => current_price <= position.stop_loss,
            Direction::Short => current_price >= position.stop_loss,
        }
    }

    fn calculate_drawdown(&self, position: &Position, current_price: U256) -> f64 {
        let entry = position.entry_price.as_u128() as f64;
        let current = current_price.as_u128() as f64;
        
        match position.direction {
            Direction::Long => {
                if current < entry {
                    (entry - current) / entry
                } else {
                    0.0
                }
            }
            Direction::Short => {
                if current > entry {
                    (current - entry) / entry
                } else {
                    0.0
                }
            }
        }
    }

    fn should_take_profit(&self, position: &Position, current_price: U256) -> bool {
        if let Some(target) = position.take_profit {
            match position.direction {
                Direction::Long => current_price >= target,
                Direction::Short => current_price <= target,
            }
        } else {
            false
        }
    }
}

struct VaRCalculator {
    confidence_level: f64,
    time_horizon: u64,
}

impl VaRCalculator {
    fn new() -> Self {
        Self {
            confidence_level: 0.95, // 95% confidence
            time_horizon: 1, // 1 day
        }
    }

    fn calculate_var(&self, portfolio_value: U256, volatility: f64) -> U256 {
        // Parametric VaR calculation
        let z_score = 1.645; // 95% confidence level
        let daily_volatility = volatility / (252.0_f64).sqrt(); // Annualized to daily
        let var_percentage = z_score * daily_volatility;
        
        portfolio_value * U256::from((var_percentage * 100.0) as u64) / U256::from(100)
    }
}

struct ExposureTracker {
    max_single_exposure: f64,
    max_sector_exposure: f64,
    max_total_exposure: f64,
}

impl ExposureTracker {
    fn new() -> Self {
        Self {
            max_single_exposure: 0.3,  // 30% max per asset
            max_sector_exposure: 0.5,   // 50% max per sector
            max_total_exposure: 0.95,   // 95% max total
        }
    }

    fn track_exposure(&self, positions: &[Position]) -> ExposureReport {
        let total_value: U256 = positions.iter().map(|p| p.size).fold(U256::zero(), |a, b| a + b);
        let mut by_token: HashMap<H256, f64> = HashMap::new();
        
        for position in positions {
            let exposure = position.size.as_u128() as f64 / total_value.as_u128().max(1) as f64;
            *by_token.entry(position.token).or_insert(0.0) += exposure;
        }
        
        ExposureReport {
            total_exposure: 1.0, // Simplified
            max_single_exposure: by_token.values().cloned().fold(0.0, f64::max),
            exposures_by_token: by_token,
        }
    }
}

struct StopLossManager {
    atr_multiplier: f64,
    max_loss_percentage: f64,
}

impl StopLossManager {
    fn new() -> Self {
        Self {
            atr_multiplier: 2.0,
            max_loss_percentage: 0.05, // 5% max loss
        }
    }

    fn calculate_stop_loss(
        &self,
        entry_price: U256,
        action: super::TradeAction,
        confidence: f64,
    ) -> U256 {
        use super::TradeAction;
        
        let stop_percentage = (self.max_loss_percentage * (2.0 - confidence)).min(0.1);
        let stop_amount = entry_price * U256::from((stop_percentage * 100.0) as u64) / U256::from(100);
        
        match action {
            TradeAction::Buy | TradeAction::Long => entry_price - stop_amount,
            TradeAction::Sell | TradeAction::Short => entry_price + stop_amount,
            _ => entry_price,
        }
    }

    fn calculate_trailing_stop(
        &self,
        current_price: U256,
        highest_price: U256,
        direction: Direction,
    ) -> U256 {
        let trail_percentage = U256::from(3); // 3% trailing stop
        let trail_amount = highest_price * trail_percentage / U256::from(100);
        
        match direction {
            Direction::Long => highest_price - trail_amount,
            Direction::Short => highest_price + trail_amount,
        }
    }
}

pub struct Portfolio {
    pub positions: Vec<Position>,
    pub total_value: U256,
    pub cash: U256,
}

pub struct Position {
    pub token: H256,
    pub size: U256,
    pub entry_price: U256,
    pub stop_loss: U256,
    pub take_profit: Option<U256>,
    pub direction: Direction,
}

#[derive(Clone)]
pub enum Direction {
    Long,
    Short,
}

pub struct RiskAlert {
    pub token: H256,
    pub alert_type: AlertType,
    pub severity: Severity,
    pub message: String,
    pub action: Option<RiskAction>,
}

pub enum AlertType {
    StopLoss,
    TakeProfit,
    Drawdown,
    Exposure,
    Volatility,
}

pub enum Severity {
    Low,
    Medium,
    High,
    Critical,
}

pub enum RiskAction {
    ClosePosition,
    ReducePosition,
    HedgePosition,
    DoNothing,
}

struct ExposureReport {
    total_exposure: f64,
    max_single_exposure: f64,
    exposures_by_token: HashMap<H256, f64>,
}