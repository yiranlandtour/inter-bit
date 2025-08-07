use super::{DeFiConfig, DeFiError};
use primitive_types::{H256, U256};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct LendingPool {
    config: DeFiConfig,
    deposits: Arc<RwLock<HashMap<([u8; 20], H256), U256>>>,
    borrows: Arc<RwLock<HashMap<([u8; 20], H256), U256>>>,
    interest_rates: Arc<RwLock<HashMap<H256, InterestRate>>>,
}

struct InterestRate {
    supply_rate: u32,
    borrow_rate: u32,
    utilization: u32,
}

impl LendingPool {
    pub fn new(config: DeFiConfig) -> Self {
        Self {
            config,
            deposits: Arc::new(RwLock::new(HashMap::new())),
            borrows: Arc::new(RwLock::new(HashMap::new())),
            interest_rates: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn deposit(&self, user: [u8; 20], token: H256, amount: U256) -> Result<U256, DeFiError> {
        let mut deposits = self.deposits.write().await;
        let balance = deposits.entry((user, token)).or_insert(U256::zero());
        *balance = *balance + amount;
        Ok(*balance)
    }

    pub async fn borrow(
        &self,
        user: [u8; 20],
        token: H256,
        amount: U256,
        _collateral_token: H256,
    ) -> Result<H256, DeFiError> {
        let mut borrows = self.borrows.write().await;
        let debt = borrows.entry((user, token)).or_insert(U256::zero());
        *debt = *debt + amount;
        Ok(H256::random())
    }

    pub async fn get_account_value(&self, user: [u8; 20]) -> Result<U256, DeFiError> {
        let deposits = self.deposits.read().await;
        let total = deposits
            .iter()
            .filter(|((u, _), _)| *u == user)
            .fold(U256::zero(), |acc, (_, v)| acc + *v);
        Ok(total)
    }
}