use super::{AIError, ModelType, PricePoint, PriceTrend};
use primitive_types::{H256, U256};

pub struct PricePredictor {
    model_type: ModelType,
}

impl PricePredictor {
    pub fn new(model_type: ModelType) -> Self {
        Self { model_type }
    }

    pub async fn predict(
        &self,
        token: H256,
        historical_prices: &[PricePoint],
        horizon: u64,
    ) -> Result<PricePrediction, AIError> {
        if historical_prices.len() < 10 {
            return Err(AIError::InsufficientData);
        }

        let current_price = historical_prices.last()
            .map(|p| p.price)
            .unwrap_or(U256::zero());

        // Simplified prediction based on moving averages
        let ma_short = self.calculate_ma(historical_prices, 5);
        let ma_long = self.calculate_ma(historical_prices, 20);
        
        let trend = if ma_short > ma_long * U256::from(102) / U256::from(100) {
            PriceTrend::Bullish
        } else if ma_short < ma_long * U256::from(98) / U256::from(100) {
            PriceTrend::Bearish
        } else {
            PriceTrend::Neutral
        };

        let predicted_price = match trend {
            PriceTrend::Bullish => current_price * U256::from(105) / U256::from(100),
            PriceTrend::Bearish => current_price * U256::from(95) / U256::from(100),
            _ => current_price,
        };

        Ok(PricePrediction {
            token,
            predicted_price,
            confidence: 0.75,
            trend,
            horizon,
        })
    }

    fn calculate_ma(&self, prices: &[PricePoint], period: usize) -> U256 {
        if prices.is_empty() {
            return U256::zero();
        }

        let start = prices.len().saturating_sub(period);
        let sum: U256 = prices[start..].iter()
            .map(|p| p.price)
            .fold(U256::zero(), |acc, p| acc + p);
        
        sum / U256::from(period.min(prices.len()))
    }
}

pub struct PricePrediction {
    pub token: H256,
    pub predicted_price: U256,
    pub confidence: f64,
    pub trend: PriceTrend,
    pub horizon: u64,
}