use super::DeFiError;
use primitive_types::{H256, U256};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum OptionType {
    Call,
    Put,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum OptionStyle {
    European,
    American,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Option {
    pub id: H256,
    pub option_type: OptionType,
    pub style: OptionStyle,
    pub underlying: H256,
    pub strike_price: U256,
    pub expiry: u64,
    pub amount: U256,
    pub writer: [u8; 20],
    pub holder: Option<[u8; 20]>,
    pub premium: U256,
    pub collateral: U256,
    pub is_exercised: bool,
    pub is_expired: bool,
}

pub struct OptionsProtocol {
    options: Arc<RwLock<HashMap<H256, Option>>>,
    pricing_engine: Arc<BlackScholesPricing>,
}

struct BlackScholesPricing {
    risk_free_rate: f64,
    volatility_oracle: Arc<RwLock<HashMap<H256, f64>>>,
}

impl OptionsProtocol {
    pub fn new() -> Self {
        Self {
            options: Arc::new(RwLock::new(HashMap::new())),
            pricing_engine: Arc::new(BlackScholesPricing {
                risk_free_rate: 0.05, // 5% annual
                volatility_oracle: Arc::new(RwLock::new(HashMap::new())),
            }),
        }
    }

    pub async fn create_option(
        &self,
        writer: [u8; 20],
        option_type: OptionType,
        underlying: H256,
        strike_price: U256,
        expiry: u64,
        amount: U256,
    ) -> Result<H256, DeFiError> {
        if expiry <= self.get_current_timestamp() {
            return Err(DeFiError::InvalidExpiry);
        }

        let premium = self.calculate_premium(
            option_type.clone(),
            underlying,
            strike_price,
            expiry,
            amount,
        ).await?;

        let collateral = match option_type {
            OptionType::Call => amount, // Need to hold underlying
            OptionType::Put => strike_price * amount / U256::from(10u64.pow(18)), // Need cash
        };

        let option_id = H256::random();
        let option = Option {
            id: option_id,
            option_type,
            style: OptionStyle::European,
            underlying,
            strike_price,
            expiry,
            amount,
            writer,
            holder: None,
            premium,
            collateral,
            is_exercised: false,
            is_expired: false,
        };

        let mut options = self.options.write().await;
        options.insert(option_id, option);

        Ok(option_id)
    }

    pub async fn buy_option(
        &self,
        buyer: [u8; 20],
        option_id: H256,
    ) -> Result<U256, DeFiError> {
        let mut options = self.options.write().await;
        let option = options.get_mut(&option_id).ok_or(DeFiError::PositionNotFound)?;

        if option.holder.is_some() {
            return Err(DeFiError::Unauthorized);
        }

        option.holder = Some(buyer);
        Ok(option.premium)
    }

    pub async fn exercise_option(
        &self,
        holder: [u8; 20],
        option_id: H256,
    ) -> Result<ExerciseResult, DeFiError> {
        let mut options = self.options.write().await;
        let option = options.get_mut(&option_id).ok_or(DeFiError::PositionNotFound)?;

        if option.holder != Some(holder) {
            return Err(DeFiError::Unauthorized);
        }

        if option.is_exercised || option.is_expired {
            return Err(DeFiError::InvalidExpiry);
        }

        let current_time = self.get_current_timestamp();
        if current_time > option.expiry {
            option.is_expired = true;
            return Err(DeFiError::InvalidExpiry);
        }

        // Get current price
        let current_price = self.get_spot_price(option.underlying).await?;
        
        // Check if profitable to exercise
        let (is_profitable, payout) = match option.option_type {
            OptionType::Call => {
                let profitable = current_price > option.strike_price;
                let payout = if profitable {
                    (current_price - option.strike_price) * option.amount / U256::from(10u64.pow(18))
                } else {
                    U256::zero()
                };
                (profitable, payout)
            }
            OptionType::Put => {
                let profitable = option.strike_price > current_price;
                let payout = if profitable {
                    (option.strike_price - current_price) * option.amount / U256::from(10u64.pow(18))
                } else {
                    U256::zero()
                };
                (profitable, payout)
            }
        };

        if !is_profitable {
            return Err(DeFiError::PositionHealthy);
        }

        option.is_exercised = true;

        Ok(ExerciseResult {
            option_id,
            payout,
            collateral_returned: option.collateral.saturating_sub(payout),
        })
    }

    pub async fn get_portfolio_value(&self, user: [u8; 20]) -> Result<U256, DeFiError> {
        let options = self.options.read().await;
        let mut total_value = U256::zero();

        for option in options.values() {
            if option.holder == Some(user) && !option.is_exercised && !option.is_expired {
                let intrinsic_value = self.calculate_intrinsic_value(option).await?;
                total_value = total_value + intrinsic_value;
            }
        }

        Ok(total_value)
    }

    async fn calculate_premium(
        &self,
        option_type: OptionType,
        underlying: H256,
        strike_price: U256,
        expiry: u64,
        amount: U256,
    ) -> Result<U256, DeFiError> {
        let spot_price = self.get_spot_price(underlying).await?;
        let time_to_expiry = (expiry - self.get_current_timestamp()) as f64 / 31536000.0; // Convert to years
        let volatility = self.pricing_engine.get_volatility(underlying).await;
        
        let premium_per_unit = self.pricing_engine.black_scholes(
            spot_price.as_u128() as f64 / 1e18,
            strike_price.as_u128() as f64 / 1e18,
            time_to_expiry,
            volatility,
            option_type == OptionType::Call,
        );

        Ok(U256::from((premium_per_unit * 1e18) as u128) * amount / U256::from(10u64.pow(18)))
    }

    async fn calculate_intrinsic_value(&self, option: &Option) -> Result<U256, DeFiError> {
        let spot_price = self.get_spot_price(option.underlying).await?;
        
        match option.option_type {
            OptionType::Call => {
                if spot_price > option.strike_price {
                    Ok((spot_price - option.strike_price) * option.amount / U256::from(10u64.pow(18)))
                } else {
                    Ok(U256::zero())
                }
            }
            OptionType::Put => {
                if option.strike_price > spot_price {
                    Ok((option.strike_price - spot_price) * option.amount / U256::from(10u64.pow(18)))
                } else {
                    Ok(U256::zero())
                }
            }
        }
    }

    async fn get_spot_price(&self, _token: H256) -> Result<U256, DeFiError> {
        // Mock price oracle
        Ok(U256::from(2000 * 10u64.pow(18))) // $2000
    }

    fn get_current_timestamp(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }
}

impl BlackScholesPricing {
    fn black_scholes(
        &self,
        spot: f64,
        strike: f64,
        time: f64,
        volatility: f64,
        is_call: bool,
    ) -> f64 {
        if time <= 0.0 {
            return if is_call {
                (spot - strike).max(0.0)
            } else {
                (strike - spot).max(0.0)
            };
        }

        let d1 = (spot.ln() - strike.ln() + (self.risk_free_rate + volatility.powi(2) / 2.0) * time)
            / (volatility * time.sqrt());
        let d2 = d1 - volatility * time.sqrt();

        let nd1 = self.normal_cdf(d1);
        let nd2 = self.normal_cdf(d2);

        if is_call {
            spot * nd1 - strike * (-self.risk_free_rate * time).exp() * nd2
        } else {
            strike * (-self.risk_free_rate * time).exp() * self.normal_cdf(-d2) 
                - spot * self.normal_cdf(-d1)
        }
    }

    fn normal_cdf(&self, x: f64) -> f64 {
        (1.0 + libm::erf(x / 2.0_f64.sqrt())) / 2.0
    }

    async fn get_volatility(&self, underlying: H256) -> f64 {
        let volatilities = self.volatility_oracle.read().await;
        *volatilities.get(&underlying).unwrap_or(&0.3) // Default 30% volatility
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExerciseResult {
    pub option_id: H256,
    pub payout: U256,
    pub collateral_returned: U256,
}