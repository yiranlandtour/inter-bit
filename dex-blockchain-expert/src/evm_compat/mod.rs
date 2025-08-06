use revm::primitives::{Address, Bytes, B256, U256 as RevmU256};
use revm::{Database, Evm, InMemoryDB};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct EvmExecutor {
    db: Arc<RwLock<InMemoryDB>>,
    gas_limit: u64,
    chain_id: u64,
}

impl EvmExecutor {
    pub fn new(chain_id: u64) -> Self {
        Self {
            db: Arc::new(RwLock::new(InMemoryDB::default())),
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
        gas_price: U256,
    ) -> Result<ExecutionResult, EvmError> {
        let mut db = self.db.write().await;
        
        let from_addr = Address::from_slice(&from);
        let to_addr = to.map(|addr| Address::from_slice(&addr));
        
        // 创建EVM实例
        let mut evm = Evm::builder()
            .with_db(&mut *db)
            .with_tx_env(|tx| {
                tx.caller = from_addr;
                tx.transact_to = if let Some(addr) = to_addr {
                    revm::primitives::TransactTo::Call(addr)
                } else {
                    revm::primitives::TransactTo::Create(revm::primitives::CreateScheme::Create)
                };
                tx.value = RevmU256::from_limbs(value.0);
                tx.data = Bytes::from(data.clone());
                tx.gas_limit = gas_limit;
                tx.gas_price = RevmU256::from_limbs(gas_price.0);
            })
            .build();
        
        // 执行交易
        let result = evm.transact_commit();
        
        match result {
            Ok(exec_result) => {
                let gas_used = exec_result.gas_used();
                let output = exec_result.output().unwrap_or_default();
                let logs = exec_result.logs().to_vec();
                
                Ok(ExecutionResult {
                    success: !exec_result.is_failure(),
                    gas_used,
                    output: output.to_vec(),
                    logs: logs.into_iter().map(|l| Log {
                        address: l.address.0.0,
                        topics: l.topics.iter().map(|t| t.0.0).collect(),
                        data: l.data.to_vec(),
                    }).collect(),
                    created_address: if to.is_none() {
                        Some(self.calculate_contract_address(from, 0))
                    } else {
                        None
                    },
                })
            }
            Err(e) => Err(EvmError::ExecutionFailed(e.to_string())),
        }
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
        value: U256,
        data: Vec<u8>,
    ) -> Result<u64, EvmError> {
        let mut low = 21000u64;
        let mut high = self.gas_limit;
        
        while low < high {
            let mid = (low + high) / 2;
            
            let result = self.execute_transaction(
                from,
                to,
                value,
                data.clone(),
                mid,
                U256::from(1),
            ).await;
            
            match result {
                Ok(_) => high = mid,
                Err(_) => low = mid + 1,
            }
        }
        
        Ok(low)
    }

    pub async fn get_code(&self, address: [u8; 20]) -> Vec<u8> {
        let db = self.db.read().await;
        let addr = Address::from_slice(&address);
        
        db.accounts.get(&addr)
            .and_then(|acc| acc.info.code.as_ref())
            .map(|code| code.bytes().to_vec())
            .unwrap_or_default()
    }

    pub async fn set_code(&self, address: [u8; 20], code: Vec<u8>) {
        let mut db = self.db.write().await;
        let addr = Address::from_slice(&address);
        
        db.insert_account_info(
            addr,
            revm::primitives::AccountInfo {
                balance: RevmU256::ZERO,
                nonce: 0,
                code_hash: B256::from_slice(&sha2::Sha256::digest(&code)),
                code: Some(revm::primitives::Bytecode::new_raw(Bytes::from(code))),
            },
        );
    }

    pub async fn get_balance(&self, address: [u8; 20]) -> U256 {
        let db = self.db.read().await;
        let addr = Address::from_slice(&address);
        
        db.accounts.get(&addr)
            .map(|acc| U256(acc.info.balance.into_limbs()))
            .unwrap_or_default()
    }

    pub async fn set_balance(&self, address: [u8; 20], balance: U256) {
        let mut db = self.db.write().await;
        let addr = Address::from_slice(&address);
        
        if let Some(acc) = db.accounts.get_mut(&addr) {
            acc.info.balance = RevmU256::from_limbs(balance.0);
        } else {
            db.insert_account_info(
                addr,
                revm::primitives::AccountInfo {
                    balance: RevmU256::from_limbs(balance.0),
                    nonce: 0,
                    code_hash: B256::ZERO,
                    code: None,
                },
            );
        }
    }

    pub async fn get_storage(&self, address: [u8; 20], key: [u8; 32]) -> [u8; 32] {
        let db = self.db.read().await;
        let addr = Address::from_slice(&address);
        let storage_key = RevmU256::from_be_bytes(key);
        
        db.accounts.get(&addr)
            .and_then(|acc| acc.storage.get(&storage_key))
            .map(|val| val.to_be_bytes())
            .unwrap_or([0; 32])
    }

    pub async fn set_storage(&self, address: [u8; 20], key: [u8; 32], value: [u8; 32]) {
        let mut db = self.db.write().await;
        let addr = Address::from_slice(&address);
        let storage_key = RevmU256::from_be_bytes(key);
        let storage_value = RevmU256::from_be_bytes(value);
        
        if let Some(acc) = db.accounts.get_mut(&addr) {
            acc.storage.insert(storage_key, storage_value);
        }
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

#[derive(Debug, Clone)]
pub struct ExecutionResult {
    pub success: bool,
    pub gas_used: u64,
    pub output: Vec<u8>,
    pub logs: Vec<Log>,
    pub created_address: Option<[u8; 20]>,
}

#[derive(Debug, Clone)]
pub struct Log {
    pub address: [u8; 20],
    pub topics: Vec<[u8; 32]>,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub enum EvmError {
    ExecutionFailed(String),
    InvalidTransaction(String),
    OutOfGas,
    Reverted(Vec<u8>),
}

use primitive_types::U256;

pub mod parallel_evm;