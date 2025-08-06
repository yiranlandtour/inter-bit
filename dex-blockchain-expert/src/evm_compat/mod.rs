use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use primitive_types::U256;
use serde::{Deserialize, Serialize};

pub struct EvmExecutor {
    storage: Arc<RwLock<HashMap<[u8; 20], AccountState>>>,
    gas_limit: u64,
    chain_id: u64,
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct AccountState {
    balance: U256,
    nonce: u64,
    code: Vec<u8>,
    storage: HashMap<[u8; 32], [u8; 32]>,
}

impl EvmExecutor {
    pub fn new(chain_id: u64) -> Self {
        Self {
            storage: Arc::new(RwLock::new(HashMap::new())),
            gas_limit: 30_000_000,
            chain_id,
        }
    }

    pub async fn execute_transaction(
        &self,
        from: [u8; 20],
        to: Option<[u8; 20]>,
        value: U256,
        data: Vec<u8>,
        gas_limit: u64,
        _gas_price: U256,
    ) -> Result<ExecutionResult, EvmError> {
        // Simplified EVM execution simulation
        let mut storage = self.storage.write().await;
        
        // Check sender balance
        let sender = storage.entry(from).or_default();
        if sender.balance < value {
            return Err(EvmError::InvalidTransaction("Insufficient balance".to_string()));
        }
        
        // Deduct value from sender
        sender.balance -= value;
        sender.nonce += 1;
        
        // Add value to recipient
        if let Some(recipient) = to {
            let receiver = storage.entry(recipient).or_default();
            receiver.balance += value;
        }
        
        // Simulate gas usage
        let gas_used = std::cmp::min(gas_limit, 21000 + data.len() as u64 * 16);
        
        Ok(ExecutionResult {
            success: true,
            gas_used,
            output: Vec::new(),
            logs: Vec::new(),
            created_address: if to.is_none() {
                Some(self.calculate_contract_address(from, sender.nonce))
            } else {
                None
            },
        })
    }

    pub async fn call(
        &self,
        from: [u8; 20],
        to: [u8; 20],
        data: Vec<u8>,
        value: U256,
    ) -> Result<Vec<u8>, EvmError> {
        let result = self.execute_transaction(
            from,
            Some(to),
            value,
            data,
            self.gas_limit,
            U256::from(1),
        ).await?;
        
        Ok(result.output)
    }

    pub async fn estimate_gas(
        &self,
        from: [u8; 20],
        to: Option<[u8; 20]>,
        _value: U256,
        data: Vec<u8>,
    ) -> Result<u64, EvmError> {
        // Simplified gas estimation
        Ok(21000 + data.len() as u64 * 16)
    }

    pub async fn get_code(&self, address: [u8; 20]) -> Vec<u8> {
        let storage = self.storage.read().await;
        storage.get(&address)
            .map(|acc| acc.code.clone())
            .unwrap_or_default()
    }

    pub async fn set_code(&self, address: [u8; 20], code: Vec<u8>) {
        let mut storage = self.storage.write().await;
        let account = storage.entry(address).or_default();
        account.code = code;
    }

    pub async fn get_balance(&self, address: [u8; 20]) -> U256 {
        let storage = self.storage.read().await;
        storage.get(&address)
            .map(|acc| acc.balance)
            .unwrap_or_default()
    }

    pub async fn set_balance(&self, address: [u8; 20], balance: U256) {
        let mut storage = self.storage.write().await;
        let account = storage.entry(address).or_default();
        account.balance = balance;
    }

    pub async fn get_storage(&self, address: [u8; 20], key: [u8; 32]) -> [u8; 32] {
        let storage = self.storage.read().await;
        storage.get(&address)
            .and_then(|acc| acc.storage.get(&key).copied())
            .unwrap_or([0; 32])
    }

    pub async fn set_storage(&self, address: [u8; 20], key: [u8; 32], value: [u8; 32]) {
        let mut storage = self.storage.write().await;
        let account = storage.entry(address).or_default();
        account.storage.insert(key, value);
    }

    fn calculate_contract_address(&self, creator: [u8; 20], nonce: u64) -> [u8; 20] {
        use sha2::{Digest, Sha256};
        
        let mut data = Vec::new();
        data.extend_from_slice(&creator);
        data.extend_from_slice(&nonce.to_be_bytes());
        
        let hash = Sha256::digest(&data);
        let mut address = [0u8; 20];
        address.copy_from_slice(&hash[12..32]);
        address
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionResult {
    pub success: bool,
    pub gas_used: u64,
    pub output: Vec<u8>,
    pub logs: Vec<Log>,
    pub created_address: Option<[u8; 20]>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Log {
    pub address: [u8; 20],
    pub topics: Vec<[u8; 32]>,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EvmError {
    ExecutionFailed(String),
    InvalidTransaction(String),
    OutOfGas,
    Reverted(Vec<u8>),
}

pub mod parallel_evm;