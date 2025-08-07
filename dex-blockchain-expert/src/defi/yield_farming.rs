use super::DeFiError;
use primitive_types::{H256, U256};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct YieldOptimizer {
    staked_balances: Arc<RwLock<HashMap<([u8; 20], H256), U256>>>,
}

impl YieldOptimizer {
    pub fn new() -> Self {
        Self {
            staked_balances: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn stake(&self, user: [u8; 20], pool_id: H256, amount: U256) -> Result<(), DeFiError> {
        let mut balances = self.staked_balances.write().await;
        let balance = balances.entry((user, pool_id)).or_insert(U256::zero());
        *balance = *balance + amount;
        Ok(())
    }

    pub async fn get_staked_value(&self, user: [u8; 20]) -> Result<U256, DeFiError> {
        let balances = self.staked_balances.read().await;
        let total = balances
            .iter()
            .filter(|((u, _), _)| *u == user)
            .fold(U256::zero(), |acc, (_, v)| acc + *v);
        Ok(total)
    }
}