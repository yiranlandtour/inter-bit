pub mod layered;
pub mod merkle_trie;
pub mod snapshot;
pub mod cache;

use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::RwLock;
use rocksdb::{DB, Options, WriteBatch};
use dashmap::DashMap;
use bytes::Bytes;

pub use layered::OptimizedStateStorage;
pub use merkle_trie::MerklePatriciaTrie;
pub use snapshot::SnapshotManager;
pub use cache::MemoryCache;

#[async_trait]
pub trait Storage: Send + Sync {
    async fn get(&self, key: &str) -> Result<Vec<u8>, StorageError>;
    async fn put(&self, key: &str, value: &[u8]) -> Result<(), StorageError>;
    async fn delete(&self, key: &str) -> Result<(), StorageError>;
    async fn batch_put(&self, items: Vec<(String, Vec<u8>)>) -> Result<(), StorageError>;
    async fn batch_delete(&self, keys: Vec<String>) -> Result<(), StorageError>;
    async fn contains(&self, key: &str) -> Result<bool, StorageError>;
    async fn iter_prefix(&self, prefix: &str) -> Result<Vec<(String, Vec<u8>)>, StorageError>;
}

#[derive(Debug, Clone)]
pub enum StorageError {
    NotFound,
    IoError(String),
    SerializationError(String),
    DatabaseError(String),
    InvalidKey(String),
}