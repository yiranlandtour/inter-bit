use super::{AIError, Allocation, Holding, OptimizedPortfolio};
use primitive_types::{H256, U256};
use std::collections::HashMap;

pub struct PortfolioOptimizer {
    optimization_method: OptimizationMethod,
}

#[derive(Clone)]
enum OptimizationMethod {
    MeanVariance,
    RiskParity,
    MaxSharpe,
    MinVolatility,
}

impl PortfolioOptimizer {
    pub fn new() -> Self {
        Self {
            optimization_method: OptimizationMethod::MaxSharpe,
        }
    }

    pub async fn optimize(
        &self,
        holdings: Vec<Holding>,
        predictions: Vec<super::predictor::PricePrediction>,
        target_return: f64,
        risk_limit: f64,
    ) -> Result<OptimizedPortfolio, AIError> {
        if holdings.is_empty() || predictions.is_empty() {
            return Err(AIError::InsufficientData);
        }

        // Calculate expected returns
        let returns = self.calculate_expected_returns(&holdings, &predictions);
        
        // Calculate covariance matrix
        let covariance = self.calculate_covariance_matrix(&holdings, &predictions);
        
        // Optimize weights based on method
        let weights = match self.optimization_method {
            OptimizationMethod::MaxSharpe => {
                self.optimize_max_sharpe(&returns, &covariance, risk_limit)
            }
            OptimizationMethod::MeanVariance => {
                self.optimize_mean_variance(&returns, &covariance, target_return, risk_limit)
            }
            OptimizationMethod::RiskParity => {
                self.optimize_risk_parity(&covariance, risk_limit)
            }
            OptimizationMethod::MinVolatility => {
                self.optimize_min_volatility(&covariance)
            }
        }?;

        // Calculate portfolio metrics
        let portfolio_return = self.calculate_portfolio_return(&returns, &weights);
        let portfolio_risk = self.calculate_portfolio_risk(&weights, &covariance);
        let sharpe_ratio = self.calculate_sharpe_ratio(portfolio_return, portfolio_risk);

        // Create allocations
        let total_value = holdings.iter()
            .map(|h| h.amount)
            .fold(U256::zero(), |acc, amt| acc + amt);
            
        let allocations = holdings.iter()
            .zip(weights.iter())
            .map(|(holding, &weight)| Allocation {
                token: holding.token,
                weight,
                amount: total_value * U256::from((weight * 1000000.0) as u64) / U256::from(1000000),
            })
            .collect();

        Ok(OptimizedPortfolio {
            allocations,
            expected_return: portfolio_return,
            risk: portfolio_risk,
            sharpe_ratio,
        })
    }

    fn calculate_expected_returns(
        &self,
        holdings: &[Holding],
        predictions: &[super::predictor::PricePrediction],
    ) -> Vec<f64> {
        holdings.iter().map(|holding| {
            predictions.iter()
                .find(|p| p.token == holding.token)
                .map(|p| {
                    let current = holding.average_price.as_u128() as f64;
                    let predicted = p.predicted_price.as_u128() as f64;
                    (predicted - current) / current
                })
                .unwrap_or(0.0)
        }).collect()
    }

    fn calculate_covariance_matrix(
        &self,
        holdings: &[Holding],
        predictions: &[super::predictor::PricePrediction],
    ) -> Vec<Vec<f64>> {
        let n = holdings.len();
        let mut covariance = vec![vec![0.0; n]; n];
        
        // Simplified covariance based on correlation assumptions
        for i in 0..n {
            for j in 0..n {
                if i == j {
                    // Variance based on prediction confidence
                    let confidence = predictions.get(i)
                        .map(|p| p.confidence)
                        .unwrap_or(0.5);
                    covariance[i][j] = (1.0 - confidence) * 0.1; // Higher confidence = lower variance
                } else {
                    // Correlation between assets
                    covariance[i][j] = 0.03; // Assumed low correlation
                }
            }
        }
        
        covariance
    }

    fn optimize_max_sharpe(
        &self,
        returns: &[f64],
        covariance: &[Vec<f64>],
        risk_limit: f64,
    ) -> Result<Vec<f64>, AIError> {
        let n = returns.len();
        let mut weights = vec![1.0 / n as f64; n];
        
        // Gradient ascent for Sharpe ratio
        let learning_rate = 0.01;
        let iterations = 100;
        
        for _ in 0..iterations {
            let portfolio_return = self.calculate_portfolio_return(returns, &weights);
            let portfolio_risk = self.calculate_portfolio_risk(&weights, covariance);
            
            if portfolio_risk > risk_limit {
                // Scale down weights if risk exceeded
                let scale = risk_limit / portfolio_risk;
                weights = weights.iter().map(|w| w * scale).collect();
            }
            
            // Update weights based on gradient
            for i in 0..n {
                let gradient = returns[i] / portfolio_risk.max(0.001);
                weights[i] += learning_rate * gradient;
                weights[i] = weights[i].max(0.0).min(1.0);
            }
            
            // Normalize weights
            let sum: f64 = weights.iter().sum();
            if sum > 0.0 {
                weights = weights.iter().map(|w| w / sum).collect();
            }
        }
        
        Ok(weights)
    }

    fn optimize_mean_variance(
        &self,
        returns: &[f64],
        covariance: &[Vec<f64>],
        target_return: f64,
        risk_limit: f64,
    ) -> Result<Vec<f64>, AIError> {
        let n = returns.len();
        let mut weights = vec![1.0 / n as f64; n];
        
        // Quadratic programming approximation
        let lambda = 0.5; // Risk aversion parameter
        
        for _ in 0..50 {
            let portfolio_return = self.calculate_portfolio_return(returns, &weights);
            let portfolio_risk = self.calculate_portfolio_risk(&weights, covariance);
            
            // Adjust weights to meet constraints
            for i in 0..n {
                let return_gradient = returns[i] - target_return;
                let risk_gradient = 2.0 * covariance[i].iter()
                    .zip(weights.iter())
                    .map(|(c, w)| c * w)
                    .sum::<f64>();
                
                weights[i] += 0.01 * (return_gradient - lambda * risk_gradient);
                weights[i] = weights[i].max(0.0).min(1.0);
            }
            
            // Normalize
            let sum: f64 = weights.iter().sum();
            if sum > 0.0 {
                weights = weights.iter().map(|w| w / sum).collect();
            }
            
            if portfolio_risk > risk_limit {
                break;
            }
        }
        
        Ok(weights)
    }

    fn optimize_risk_parity(
        &self,
        covariance: &[Vec<f64>],
        risk_limit: f64,
    ) -> Result<Vec<f64>, AIError> {
        let n = covariance.len();
        let mut weights = vec![1.0 / n as f64; n];
        
        // Equal risk contribution
        for _ in 0..50 {
            let mut risk_contributions = vec![0.0; n];
            let portfolio_risk = self.calculate_portfolio_risk(&weights, covariance);
            
            for i in 0..n {
                risk_contributions[i] = weights[i] * covariance[i].iter()
                    .zip(weights.iter())
                    .map(|(c, w)| c * w)
                    .sum::<f64>() / portfolio_risk.max(0.001);
            }
            
            // Adjust weights to equalize risk contributions
            let target_contribution = 1.0 / n as f64;
            for i in 0..n {
                weights[i] *= target_contribution / risk_contributions[i].max(0.001);
                weights[i] = weights[i].max(0.0).min(1.0);
            }
            
            // Normalize
            let sum: f64 = weights.iter().sum();
            if sum > 0.0 {
                weights = weights.iter().map(|w| w / sum).collect();
            }
            
            if portfolio_risk > risk_limit {
                let scale = risk_limit / portfolio_risk;
                weights = weights.iter().map(|w| w * scale).collect();
            }
        }
        
        Ok(weights)
    }

    fn optimize_min_volatility(
        &self,
        covariance: &[Vec<f64>],
    ) -> Result<Vec<f64>, AIError> {
        let n = covariance.len();
        let mut weights = vec![1.0 / n as f64; n];
        
        // Minimize portfolio variance
        for _ in 0..100 {
            let mut gradients = vec![0.0; n];
            
            for i in 0..n {
                gradients[i] = 2.0 * covariance[i].iter()
                    .zip(weights.iter())
                    .map(|(c, w)| c * w)
                    .sum::<f64>();
            }
            
            // Update weights
            for i in 0..n {
                weights[i] -= 0.01 * gradients[i];
                weights[i] = weights[i].max(0.0).min(1.0);
            }
            
            // Normalize
            let sum: f64 = weights.iter().sum();
            if sum > 0.0 {
                weights = weights.iter().map(|w| w / sum).collect();
            }
        }
        
        Ok(weights)
    }

    fn calculate_portfolio_return(&self, returns: &[f64], weights: &[f64]) -> f64 {
        returns.iter()
            .zip(weights.iter())
            .map(|(r, w)| r * w)
            .sum()
    }

    fn calculate_portfolio_risk(&self, weights: &[f64], covariance: &[Vec<f64>]) -> f64 {
        let variance: f64 = weights.iter().enumerate()
            .map(|(i, wi)| {
                weights.iter().enumerate()
                    .map(|(j, wj)| wi * wj * covariance[i][j])
                    .sum::<f64>()
            })
            .sum();
        
        variance.sqrt()
    }

    fn calculate_sharpe_ratio(&self, return_rate: f64, risk: f64) -> f64 {
        let risk_free_rate = 0.02; // 2% risk-free rate
        if risk > 0.0 {
            (return_rate - risk_free_rate) / risk
        } else {
            0.0
        }
    }

    pub async fn rebalance(
        &self,
        current_portfolio: OptimizedPortfolio,
        market_conditions: MarketConditions,
    ) -> Result<Vec<RebalanceAction>, AIError> {
        let mut actions = Vec::new();
        
        // Calculate deviation from target weights
        for allocation in &current_portfolio.allocations {
            let current_weight = allocation.weight;
            let target_weight = self.calculate_target_weight(
                &allocation.token,
                &market_conditions,
            );
            
            let deviation = (target_weight - current_weight).abs();
            
            // Rebalance if deviation exceeds threshold
            if deviation > 0.02 { // 2% threshold
                actions.push(RebalanceAction {
                    token: allocation.token,
                    current_weight,
                    target_weight,
                    action: if target_weight > current_weight {
                        ActionType::Buy
                    } else {
                        ActionType::Sell
                    },
                    amount: allocation.amount * U256::from((deviation * 1000000.0) as u64) / U256::from(1000000),
                });
            }
        }
        
        Ok(actions)
    }

    fn calculate_target_weight(
        &self,
        token: &H256,
        conditions: &MarketConditions,
    ) -> f64 {
        // Dynamic weight calculation based on market conditions
        let base_weight = 1.0 / conditions.num_assets as f64;
        let volatility_adjustment = 1.0 - conditions.volatility.min(1.0) * 0.5;
        let momentum_adjustment = 1.0 + conditions.momentum * 0.2;
        
        base_weight * volatility_adjustment * momentum_adjustment
    }
}

#[derive(Clone)]
pub struct MarketConditions {
    pub volatility: f64,
    pub momentum: f64,
    pub correlation: f64,
    pub num_assets: usize,
}

pub struct RebalanceAction {
    pub token: H256,
    pub current_weight: f64,
    pub target_weight: f64,
    pub action: ActionType,
    pub amount: U256,
}

pub enum ActionType {
    Buy,
    Sell,
    Hold,
}