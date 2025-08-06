use primitive_types::{H256, U256};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use dashmap::DashMap;
use crate::storage::OptimizedStateStorage;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Account {
    pub nonce: u64,
    pub balance: U256,
    pub storage_root: H256,
    pub code_hash: Option<H256>,
}

pub struct StateDB {
    accounts: Arc<DashMap<[u8; 20], Account>>,
    storage: Arc<DashMap<(H256, H256), H256>>, // (address_hash, key) -> value
    code: Arc<DashMap<H256, Vec<u8>>>,
    dirty_accounts: Arc<RwLock<HashMap<[u8; 20], Account>>>,
    backend: Arc<OptimizedStateStorage>,
    cache_size: usize,
}

impl StateDB {
    pub fn new(backend: Arc<OptimizedStateStorage>) -> Self {
        Self {
            accounts: Arc::new(DashMap::new()),
            storage: Arc::new(DashMap::new()),
            code: Arc::new(DashMap::new()),
            dirty_accounts: Arc::new(RwLock::new(HashMap::new())),
            backend,
            cache_size: 10000,
        }
    }

    pub async fn get_account(&self, address: [u8; 20]) -> Option<Account> {
        // 先查缓存
        if let Some(account) = self.accounts.get(&address) {
            return Some(account.clone());
        }

        // 从后端存储加载
        if let Ok(data) = self.backend.get(&Self::account_key(address)).await {
            if let Ok(account) = bincode::deserialize::<Account>(&data) {
                self.accounts.insert(address, account.clone());
                return Some(account);
            }
        }

        None
    }

    pub async fn set_account(&self, address: [u8; 20], account: Account) {
        self.accounts.insert(address, account.clone());
        
        let mut dirty = self.dirty_accounts.write().await;
        dirty.insert(address, account);
        
        // 如果缓存过大，触发清理
        if self.accounts.len() > self.cache_size {
            self.evict_cache().await;
        }
    }

    pub async fn get_storage(&self, address: [u8; 20], key: H256) -> H256 {
        let mut address_bytes = [0u8; 32];
        address_bytes[12..32].copy_from_slice(&address);
        let address_hash = H256::from(address_bytes);
        let cache_key = (address_hash, key);
        
        // 查缓存
        if let Some(value) = self.storage.get(&cache_key) {
            return *value;
        }

        // 从后端加载
        let storage_key = Self::storage_key(address, key);
        if let Ok(data) = self.backend.get(&storage_key).await {
            if data.len() == 32 {
                let mut value_bytes = [0u8; 32];
                value_bytes.copy_from_slice(&data);
                let value = H256::from(value_bytes);
                self.storage.insert(cache_key, value);
                return value;
            }
        }

        H256::zero()
    }

    pub async fn set_storage(&self, address: [u8; 20], key: H256, value: H256) {
        let mut address_bytes = [0u8; 32];
        address_bytes[12..32].copy_from_slice(&address);
        let address_hash = H256::from(address_bytes);
        self.storage.insert((address_hash, key), value);
    }

    pub async fn get_code(&self, code_hash: H256) -> Option<Vec<u8>> {
        // 查缓存
        if let Some(code) = self.code.get(&code_hash) {
            return Some(code.clone());
        }

        // 从后端加载
        let code_key = Self::code_key(code_hash);
        if let Ok(code) = self.backend.get(&code_key).await {
            self.code.insert(code_hash, code.clone());
            return Some(code);
        }

        None
    }

    pub async fn set_code(&self, address: [u8; 20], code: Vec<u8>) -> H256 {
        use sha2::{Digest, Sha256};
        
        let hash_bytes = Sha256::digest(&code);
        let code_hash = H256::from_slice(&hash_bytes);
        
        self.code.insert(code_hash, code.clone());
        
        // 更新账户的代码哈希
        if let Some(mut account) = self.get_account(address).await {
            account.code_hash = Some(code_hash);
            self.set_account(address, account).await;
        }
        
        code_hash
    }

    pub async fn is_contract(&self, address: [u8; 20]) -> bool {
        self.get_account(address).await
            .map(|acc| acc.code_hash.is_some())
            .unwrap_or(false)
    }

    pub async fn commit(&self, block_hash: H256) -> Result<(), Box<dyn std::error::Error>> {
        let dirty = self.dirty_accounts.read().await;
        
        // 批量写入脏账户
        let mut batch = Vec::new();
        for (address, account) in dirty.iter() {
            let key = Self::account_key(*address);
            let value = bincode::serialize(account)?;
            batch.push((key, value));
        }
        
        // 批量写入存储变更
        for entry in self.storage.iter() {
            let ((address_hash, key), value) = entry.pair();
            let storage_key = Self::storage_key_from_hash(*address_hash, *key);
            batch.push((storage_key, value.as_bytes().to_vec()));
        }
        
        // 批量写入代码
        for entry in self.code.iter() {
            let (hash, code) = entry.pair();
            let code_key = Self::code_key(*hash);
            batch.push((code_key, code.clone()));
        }
        
        // 批量提交到后端存储
        self.backend.batch_put(batch).await?;
        
        // 记录区块哈希
        self.backend.put(
            &format!("block:{}", hex::encode(block_hash)),
            &block_hash.as_bytes().to_vec()
        ).await?;
        
        // 清理脏数据
        drop(dirty);
        self.dirty_accounts.write().await.clear();
        
        Ok(())
    }

    pub async fn rollback(&self, block_hash: H256) -> Result<(), Box<dyn std::error::Error>> {
        // 清理缓存
        self.accounts.clear();
        self.storage.clear();
        self.dirty_accounts.write().await.clear();
        
        // 从指定区块重新加载状态
        // 这里简化处理，实际需要维护状态快照
        Ok(())
    }

    async fn evict_cache(&self) {
        // 简单的LRU缓存清理
        let to_remove = self.cache_size / 10;
        let mut removed = 0;
        
        for entry in self.accounts.iter() {
            if removed >= to_remove {
                break;
            }
            // 这里应该根据访问时间来决定驱逐哪些条目
            // 简化处理：随机驱逐
            if rand::random::<bool>() {
                self.accounts.remove(entry.key());
                removed += 1;
            }
        }
    }

    fn account_key(address: [u8; 20]) -> String {
        format!("account:{}", hex::encode(address))
    }

    fn storage_key(address: [u8; 20], key: H256) -> String {
        format!("storage:{}:{}", hex::encode(address), hex::encode(key))
    }

    fn storage_key_from_hash(address_hash: H256, key: H256) -> String {
        format!("storage:{}:{}", hex::encode(address_hash), hex::encode(key))
    }

    fn code_key(code_hash: H256) -> String {
        format!("code:{}", hex::encode(code_hash))
    }

    pub async fn get_account_count(&self) -> usize {
        self.accounts.len()
    }

    pub async fn get_storage_count(&self) -> usize {
        self.storage.len()
    }

    pub async fn get_code_count(&self) -> usize {
        self.code.len()
    }

    pub async fn create_snapshot(&self) -> StateSnapshot {
        StateSnapshot {
            accounts: self.accounts.iter().map(|e| (*e.key(), e.value().clone())).collect(),
            storage: self.storage.iter().map(|e| (*e.key(), *e.value())).collect(),
            code: self.code.iter().map(|e| (*e.key(), e.value().clone())).collect(),
        }
    }

    pub async fn restore_snapshot(&self, snapshot: StateSnapshot) {
        self.accounts.clear();
        self.storage.clear();
        self.code.clear();
        
        for (address, account) in snapshot.accounts {
            self.accounts.insert(address, account);
        }
        
        for (key, value) in snapshot.storage {
            self.storage.insert(key, value);
        }
        
        for (hash, code) in snapshot.code {
            self.code.insert(hash, code);
        }
    }
}

#[derive(Debug, Clone)]
pub struct StateSnapshot {
    pub accounts: Vec<([u8; 20], Account)>,
    pub storage: Vec<((H256, H256), H256)>,
    pub code: Vec<(H256, Vec<u8>)>,
}

impl Account {
    pub fn new(balance: U256) -> Self {
        Self {
            nonce: 0,
            balance,
            storage_root: H256::zero(),
            code_hash: None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.nonce == 0 && 
        self.balance == U256::zero() && 
        self.code_hash.is_none()
    }

    pub fn is_contract(&self) -> bool {
        self.code_hash.is_some()
    }
}