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
    async fn execute_block(&self, transactions: Vec<Transaction>) -> Result<Vec<TransactionReceipt>, Error>;
    async fn get_state(&self, address: [u8; 20]) -> Option<Account>;
    async fn get_storage(&self, address: [u8; 20], key: H256) -> H256;
    async fn commit_state(&self, block_hash: H256) -> Result<(), Error>;
    async fn rollback_state(&self, block_hash: H256) -> Result<(), Error>;
}

#[derive(Debug, Clone)]
pub enum Error {
    InvalidTransaction(String),
    InsufficientBalance,
    GasLimitExceeded,
    StateNotFound,
    ExecutionFailed(String),
    StorageError(String),
}