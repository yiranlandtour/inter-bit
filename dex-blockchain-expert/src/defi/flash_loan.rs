use super::DeFiError;
use primitive_types::{H256, U256};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct FlashLoanProvider {
    fee_bps: u32,
    liquidity_pools: Arc<RwLock<HashMap<H256, LiquidityPool>>>,
    active_loans: Arc<RwLock<HashMap<H256, FlashLoan>>>,
    loan_history: Arc<RwLock<Vec<LoanRecord>>>,
}

#[derive(Debug, Clone)]
struct LiquidityPool {
    token: H256,
    available_balance: U256,
    total_balance: U256,
    utilization_rate: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FlashLoan {
    id: H256,
    borrower: [u8; 20],
    token: H256,
    amount: U256,
    fee: U256,
    callback_data: Vec<u8>,
    initiated_at: u64,
    status: LoanStatus,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
enum LoanStatus {
    Active,
    Repaid,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LoanRecord {
    loan_id: H256,
    borrower: [u8; 20],
    token: H256,
    amount: U256,
    fee: U256,
    timestamp: u64,
    success: bool,
}

impl FlashLoanProvider {
    pub fn new(fee_bps: u32) -> Self {
        Self {
            fee_bps,
            liquidity_pools: Arc::new(RwLock::new(HashMap::new())),
            active_loans: Arc::new(RwLock::new(HashMap::new())),
            loan_history: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub async fn execute_loan(
        &self,
        borrower: [u8; 20],
        token: H256,
        amount: U256,
        callback_data: Vec<u8>,
    ) -> Result<H256, DeFiError> {
        // Check available liquidity
        let mut pools = self.liquidity_pools.write().await;
        let pool = pools.get_mut(&token).ok_or(DeFiError::InsufficientBalance)?;
        
        if pool.available_balance < amount {
            return Err(DeFiError::InsufficientBalance);
        }

        // Calculate fee
        let fee = amount * U256::from(self.fee_bps) / U256::from(10000);
        
        // Create loan
        let loan_id = H256::random();
        let loan = FlashLoan {
            id: loan_id,
            borrower,
            token,
            amount,
            fee,
            callback_data: callback_data.clone(),
            initiated_at: self.get_current_timestamp(),
            status: LoanStatus::Active,
        };

        // Update pool balance
        pool.available_balance = pool.available_balance - amount;
        
        // Store active loan
        let mut active = self.active_loans.write().await;
        active.insert(loan_id, loan.clone());
        
        drop(pools);
        drop(active);

        // Execute callback (simulated)
        let callback_result = self.execute_callback(
            borrower,
            amount,
            fee,
            callback_data,
        ).await;

        // Check repayment
        let repayment_success = match callback_result {
            Ok(repaid_amount) => {
                if repaid_amount >= amount + fee {
                    self.complete_repayment(loan_id, repaid_amount).await?;
                    true
                } else {
                    false
                }
            }
            Err(_) => false,
        };

        // Update loan status
        let mut active = self.active_loans.write().await;
        if let Some(loan) = active.get_mut(&loan_id) {
            loan.status = if repayment_success {
                LoanStatus::Repaid
            } else {
                LoanStatus::Failed
            };
        }

        // Record loan
        let mut history = self.loan_history.write().await;
        history.push(LoanRecord {
            loan_id,
            borrower,
            token,
            amount,
            fee,
            timestamp: self.get_current_timestamp(),
            success: repayment_success,
        });

        if !repayment_success {
            return Err(DeFiError::FlashLoanNotRepaid);
        }

        Ok(loan_id)
    }

    async fn execute_callback(
        &self,
        _borrower: [u8; 20],
        amount: U256,
        fee: U256,
        _callback_data: Vec<u8>,
    ) -> Result<U256, DeFiError> {
        // Simulate callback execution
        // In a real implementation, this would call the borrower's contract
        
        // For testing, assume successful arbitrage that generates profit
        let profit = amount * U256::from(200) / U256::from(10000); // 2% profit
        Ok(amount + fee + profit)
    }

    async fn complete_repayment(
        &self,
        loan_id: H256,
        repaid_amount: U256,
    ) -> Result<(), DeFiError> {
        let active = self.active_loans.read().await;
        let loan = active.get(&loan_id).ok_or(DeFiError::PositionNotFound)?;
        
        let expected_repayment = loan.amount + loan.fee;
        if repaid_amount < expected_repayment {
            return Err(DeFiError::FlashLoanNotRepaid);
        }

        // Return funds to pool
        let mut pools = self.liquidity_pools.write().await;
        if let Some(pool) = pools.get_mut(&loan.token) {
            pool.available_balance = pool.available_balance + repaid_amount;
            pool.total_balance = pool.total_balance + loan.fee; // Protocol keeps the fee
        }

        Ok(())
    }

    pub async fn add_liquidity(
        &self,
        token: H256,
        amount: U256,
    ) -> Result<(), DeFiError> {
        let mut pools = self.liquidity_pools.write().await;
        
        pools.entry(token)
            .and_modify(|pool| {
                pool.available_balance = pool.available_balance + amount;
                pool.total_balance = pool.total_balance + amount;
            })
            .or_insert(LiquidityPool {
                token,
                available_balance: amount,
                total_balance: amount,
                utilization_rate: 0,
            });

        Ok(())
    }

    pub async fn remove_liquidity(
        &self,
        token: H256,
        amount: U256,
    ) -> Result<(), DeFiError> {
        let mut pools = self.liquidity_pools.write().await;
        let pool = pools.get_mut(&token).ok_or(DeFiError::InsufficientBalance)?;
        
        if pool.available_balance < amount {
            return Err(DeFiError::InsufficientBalance);
        }

        pool.available_balance = pool.available_balance - amount;
        pool.total_balance = pool.total_balance - amount;

        Ok(())
    }

    pub async fn get_available_liquidity(&self, token: H256) -> U256 {
        let pools = self.liquidity_pools.read().await;
        pools.get(&token)
            .map(|pool| pool.available_balance)
            .unwrap_or(U256::zero())
    }

    pub async fn get_loan_stats(&self) -> FlashLoanStats {
        let history = self.loan_history.read().await;
        let active = self.active_loans.read().await;
        
        let total_loans = history.len();
        let successful_loans = history.iter().filter(|r| r.success).count();
        let total_volume = history.iter().fold(U256::zero(), |acc, r| acc + r.amount);
        let total_fees = history.iter().fold(U256::zero(), |acc, r| acc + r.fee);
        
        FlashLoanStats {
            total_loans,
            successful_loans,
            failed_loans: total_loans - successful_loans,
            active_loans: active.len(),
            total_volume,
            total_fees_collected: total_fees,
        }
    }

    fn get_current_timestamp(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlashLoanStats {
    pub total_loans: usize,
    pub successful_loans: usize,
    pub failed_loans: usize,
    pub active_loans: usize,
    pub total_volume: U256,
    pub total_fees_collected: U256,
}

// Flash loan callback interface
#[async_trait::async_trait]
pub trait IFlashLoanReceiver {
    async fn execute_operation(
        &self,
        token: H256,
        amount: U256,
        fee: U256,
        params: Vec<u8>,
    ) -> Result<(), DeFiError>;
}

// Example flash loan use case: Arbitrage
pub struct ArbitrageBot {
    dex_a: H256,
    dex_b: H256,
}

#[async_trait::async_trait]
impl IFlashLoanReceiver for ArbitrageBot {
    async fn execute_operation(
        &self,
        token: H256,
        amount: U256,
        fee: U256,
        _params: Vec<u8>,
    ) -> Result<(), DeFiError> {
        // 1. Use borrowed funds to buy token on DEX A (cheaper)
        let buy_price = U256::from(1000 * 10u64.pow(18));
        let tokens_bought = amount * U256::from(10100) / buy_price; // 1% price difference
        
        // 2. Sell tokens on DEX B (more expensive)
        let sell_price = U256::from(1010 * 10u64.pow(18));
        let proceeds = tokens_bought * sell_price / U256::from(10000);
        
        // 3. Repay flash loan + fee
        let repayment = amount + fee;
        
        if proceeds >= repayment {
            // Profit = proceeds - repayment
            Ok(())
        } else {
            Err(DeFiError::InsufficientBalance)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_flash_loan_successful() {
        let provider = FlashLoanProvider::new(9); // 0.09% fee
        
        let token = H256::random();
        let liquidity = U256::from(1_000_000 * 10u64.pow(18));
        
        // Add liquidity
        provider.add_liquidity(token, liquidity).await.unwrap();
        
        // Execute flash loan
        let borrower = [1u8; 20];
        let loan_amount = U256::from(100_000 * 10u64.pow(18));
        
        let result = provider.execute_loan(
            borrower,
            token,
            loan_amount,
            vec![],
        ).await;
        
        assert!(result.is_ok());
        
        // Check stats
        let stats = provider.get_loan_stats().await;
        assert_eq!(stats.successful_loans, 1);
        assert_eq!(stats.failed_loans, 0);
    }

    #[tokio::test]
    async fn test_flash_loan_insufficient_liquidity() {
        let provider = FlashLoanProvider::new(9);
        
        let token = H256::random();
        let liquidity = U256::from(1000 * 10u64.pow(18));
        
        provider.add_liquidity(token, liquidity).await.unwrap();
        
        let borrower = [1u8; 20];
        let loan_amount = U256::from(2000 * 10u64.pow(18)); // More than available
        
        let result = provider.execute_loan(
            borrower,
            token,
            loan_amount,
            vec![],
        ).await;
        
        assert!(matches!(result, Err(DeFiError::InsufficientBalance)));
    }
}