pub mod executor;
pub mod parallel;
pub mod transaction;
pub mod state;

use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::RwLock;
use dashmap::DashMap;
use primitive_types::{H256, U256};

pub use executor::HighPerformanceStateMachine;
pub use parallel::ParallelExecutor;
pub use transaction::{Transaction, TransactionReceipt};
pub use state::{Account, StateDB};

#[async_trait]
pub trait StateMachine: Send + Sync {
    async fn execute_block(&self, transactions: Vec<Transaction>) -> Result<Vec<TransactionReceipt>, StateMachineError>;
    async fn get_state(&self, address: [u8; 20]) -> Option<Account>;
    async fn get_storage(&self, address: [u8; 20], key: H256) -> H256;
    async fn commit_state(&self, block_hash: H256) -> Result<(), StateMachineError>;
    async fn rollback_state(&self, block_hash: H256) -> Result<(), StateMachineError>;
}

#[derive(Debug, Clone, PartialEq)]
pub enum StateMachineError {
    InvalidTransaction(String),
    InsufficientBalance,
    GasLimitExceeded,
    StateNotFound,
    ExecutionFailed(String),
    StorageError(String),
}

impl std::fmt::Display for StateMachineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StateMachineError::InvalidTransaction(msg) => write!(f, "Invalid transaction: {}", msg),
            StateMachineError::InsufficientBalance => write!(f, "Insufficient balance"),
            StateMachineError::GasLimitExceeded => write!(f, "Gas limit exceeded"),
            StateMachineError::StateNotFound => write!(f, "State not found"),
            StateMachineError::ExecutionFailed(msg) => write!(f, "Execution failed: {}", msg),
            StateMachineError::StorageError(msg) => write!(f, "Storage error: {}", msg),
        }
    }
}

impl std::error::Error for StateMachineError {}