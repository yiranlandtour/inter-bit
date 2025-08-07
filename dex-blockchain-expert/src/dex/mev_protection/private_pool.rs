use super::{MevProtectionError, ProtectedTransaction};
use primitive_types::{H256, U256};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrivateBundle {
    pub id: H256,
    pub transactions: Vec<ProtectedTransaction>,
    pub min_block: u64,
    pub max_block: u64,
    pub searcher: [u8; 20],
    pub bid_amount: U256,
    pub revert_protection: bool,
}

#[derive(Debug, Clone)]
pub struct SearcherProfile {
    pub address: [u8; 20],
    pub reputation_score: u32,
    pub total_bundles_submitted: u64,
    pub successful_bundles: u64,
    pub total_bid_value: U256,
    pub is_whitelisted: bool,
    pub last_activity: u64,
}

#[derive(Debug, Clone)]
pub struct PrivatePoolConfig {
    pub min_reputation_score: u32,
    pub max_bundle_size: usize,
    pub bundle_timeout: u64,
    pub min_bid_amount: U256,
    pub whitelist_only: bool,
}

impl Default for PrivatePoolConfig {
    fn default() -> Self {
        Self {
            min_reputation_score: 50,
            max_bundle_size: 20,
            bundle_timeout: 100,
            min_bid_amount: U256::from(1_000_000_000u64),
            whitelist_only: false,
        }
    }
}

pub struct PrivateTransactionPool {
    config: PrivatePoolConfig,
    private_bundles: Arc<RwLock<HashMap<H256, PrivateBundle>>>,
    pending_queue: Arc<RwLock<VecDeque<PrivateBundle>>>,
    searcher_profiles: Arc<RwLock<HashMap<[u8; 20], SearcherProfile>>>,
    whitelist: Arc<RwLock<HashSet<[u8; 20]>>>,
    bundle_results: Arc<RwLock<HashMap<H256, BundleResult>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleResult {
    pub bundle_id: H256,
    pub block_number: u64,
    pub gas_used: U256,
    pub profit: U256,
    pub reverted: bool,
    pub inclusion_index: usize,
}

impl PrivateTransactionPool {
    pub fn new() -> Self {
        Self::with_config(PrivatePoolConfig::default())
    }

    pub fn with_config(config: PrivatePoolConfig) -> Self {
        Self {
            config,
            private_bundles: Arc::new(RwLock::new(HashMap::new())),
            pending_queue: Arc::new(RwLock::new(VecDeque::new())),
            searcher_profiles: Arc::new(RwLock::new(HashMap::new())),
            whitelist: Arc::new(RwLock::new(HashSet::new())),
            bundle_results: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn submit_private(
        &self,
        tx: ProtectedTransaction,
    ) -> Result<H256, MevProtectionError> {
        if !self.verify_access(&tx.sender).await? {
            return Err(MevProtectionError::PrivatePoolAccessDenied);
        }

        let bundle = PrivateBundle {
            id: H256::random(),
            transactions: vec![tx.clone()],
            min_block: 0,
            max_block: u64::MAX,
            searcher: tx.sender,
            bid_amount: tx.gas_price,
            revert_protection: true,
        };

        self.add_bundle(bundle.clone()).await?;
        Ok(bundle.id)
    }

    pub async fn submit_bundle(&self, bundle: PrivateBundle) -> Result<H256, MevProtectionError> {
        if !self.verify_access(&bundle.searcher).await? {
            return Err(MevProtectionError::PrivatePoolAccessDenied);
        }

        if bundle.transactions.len() > self.config.max_bundle_size {
            return Err(MevProtectionError::BatchAuctionFailed(
                "Bundle exceeds maximum size".to_string(),
            ));
        }

        if bundle.bid_amount < self.config.min_bid_amount {
            return Err(MevProtectionError::BatchAuctionFailed(
                "Bid amount below minimum".to_string(),
            ));
        }

        self.add_bundle(bundle.clone()).await?;
        self.update_searcher_stats(&bundle.searcher, &bundle).await;
        
        Ok(bundle.id)
    }

    pub async fn get_next_bundles(&self, block_number: u64) -> Vec<PrivateBundle> {
        let mut queue = self.pending_queue.write().await;
        let mut selected = Vec::new();
        let mut temp_queue = VecDeque::new();

        while let Some(bundle) = queue.pop_front() {
            if block_number >= bundle.min_block && block_number <= bundle.max_block {
                selected.push(bundle);
            } else if block_number < bundle.min_block {
                temp_queue.push_back(bundle);
            }
        }

        queue.append(&mut temp_queue);
        
        selected.sort_by(|a, b| b.bid_amount.cmp(&a.bid_amount));
        selected
    }

    pub async fn report_bundle_result(&self, result: BundleResult) -> Result<(), MevProtectionError> {
        let mut results = self.bundle_results.write().await;
        results.insert(result.bundle_id, result.clone());

        let bundles = self.private_bundles.read().await;
        if let Some(bundle) = bundles.get(&result.bundle_id) {
            self.update_searcher_reputation(&bundle.searcher, &result).await;
        }

        Ok(())
    }

    pub async fn add_to_whitelist(&self, searcher: [u8; 20]) -> Result<(), MevProtectionError> {
        let mut whitelist = self.whitelist.write().await;
        whitelist.insert(searcher);
        
        let mut profiles = self.searcher_profiles.write().await;
        profiles
            .entry(searcher)
            .or_insert_with(|| SearcherProfile {
                address: searcher,
                reputation_score: 100,
                total_bundles_submitted: 0,
                successful_bundles: 0,
                total_bid_value: U256::zero(),
                is_whitelisted: true,
                last_activity: self.get_current_timestamp(),
            })
            .is_whitelisted = true;

        Ok(())
    }

    pub async fn remove_from_whitelist(&self, searcher: [u8; 20]) -> Result<(), MevProtectionError> {
        let mut whitelist = self.whitelist.write().await;
        whitelist.remove(&searcher);
        
        let mut profiles = self.searcher_profiles.write().await;
        if let Some(profile) = profiles.get_mut(&searcher) {
            profile.is_whitelisted = false;
        }

        Ok(())
    }

    pub async fn get_searcher_profile(&self, searcher: [u8; 20]) -> Option<SearcherProfile> {
        let profiles = self.searcher_profiles.read().await;
        profiles.get(&searcher).cloned()
    }

    async fn verify_access(&self, searcher: &[u8; 20]) -> Result<bool, MevProtectionError> {
        if self.config.whitelist_only {
            let whitelist = self.whitelist.read().await;
            return Ok(whitelist.contains(searcher));
        }

        let profiles = self.searcher_profiles.read().await;
        if let Some(profile) = profiles.get(searcher) {
            Ok(profile.reputation_score >= self.config.min_reputation_score)
        } else {
            Ok(!self.config.whitelist_only)
        }
    }

    async fn add_bundle(&self, bundle: PrivateBundle) -> Result<(), MevProtectionError> {
        let mut bundles = self.private_bundles.write().await;
        bundles.insert(bundle.id, bundle.clone());
        
        let mut queue = self.pending_queue.write().await;
        
        let insert_pos = queue
            .iter()
            .position(|b| b.bid_amount < bundle.bid_amount)
            .unwrap_or(queue.len());
        
        queue.insert(insert_pos, bundle);
        
        Ok(())
    }

    async fn update_searcher_stats(&self, searcher: &[u8; 20], bundle: &PrivateBundle) {
        let mut profiles = self.searcher_profiles.write().await;
        let profile = profiles
            .entry(*searcher)
            .or_insert_with(|| SearcherProfile {
                address: *searcher,
                reputation_score: 50,
                total_bundles_submitted: 0,
                successful_bundles: 0,
                total_bid_value: U256::zero(),
                is_whitelisted: false,
                last_activity: 0,
            });

        profile.total_bundles_submitted += 1;
        profile.total_bid_value = profile.total_bid_value + bundle.bid_amount;
        profile.last_activity = self.get_current_timestamp();
    }

    async fn update_searcher_reputation(&self, searcher: &[u8; 20], result: &BundleResult) {
        let mut profiles = self.searcher_profiles.write().await;
        if let Some(profile) = profiles.get_mut(searcher) {
            if !result.reverted {
                profile.successful_bundles += 1;
                profile.reputation_score = std::cmp::min(100, profile.reputation_score + 2);
            } else {
                profile.reputation_score = profile.reputation_score.saturating_sub(5);
            }
        }
    }

    pub async fn simulate_bundle(&self, bundle: &PrivateBundle) -> Result<BundleSimulation, MevProtectionError> {
        let mut total_gas = U256::zero();
        let mut expected_profit = U256::zero();
        let mut simulation_results = Vec::new();

        for tx in &bundle.transactions {
            let gas_estimate = self.estimate_gas(tx).await;
            total_gas = total_gas + gas_estimate;
            
            simulation_results.push(TxSimulation {
                tx_hash: tx.tx_hash,
                gas_used: gas_estimate,
                success: true,
            });
        }

        expected_profit = bundle.bid_amount.saturating_sub(total_gas * U256::from(20_000_000_000u64));

        Ok(BundleSimulation {
            bundle_id: bundle.id,
            total_gas_used: total_gas,
            expected_profit,
            simulated_txs: simulation_results,
            would_revert: false,
        })
    }

    async fn estimate_gas(&self, _tx: &ProtectedTransaction) -> U256 {
        U256::from(21000u64)
    }

    fn get_current_timestamp(&self) -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    pub async fn get_pool_stats(&self) -> PoolStats {
        let bundles = self.private_bundles.read().await;
        let queue = self.pending_queue.read().await;
        let profiles = self.searcher_profiles.read().await;

        PoolStats {
            total_bundles: bundles.len(),
            pending_bundles: queue.len(),
            total_searchers: profiles.len(),
            whitelisted_searchers: profiles.values().filter(|p| p.is_whitelisted).count(),
            total_bid_value: queue.iter().fold(U256::zero(), |acc, b| acc + b.bid_amount),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleSimulation {
    pub bundle_id: H256,
    pub total_gas_used: U256,
    pub expected_profit: U256,
    pub simulated_txs: Vec<TxSimulation>,
    pub would_revert: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxSimulation {
    pub tx_hash: H256,
    pub gas_used: U256,
    pub success: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolStats {
    pub total_bundles: usize,
    pub pending_bundles: usize,
    pub total_searchers: usize,
    pub whitelisted_searchers: usize,
    pub total_bid_value: U256,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_private_pool_submission() {
        let pool = PrivateTransactionPool::new();
        
        let tx = ProtectedTransaction {
            tx_hash: H256::random(),
            sender: [1u8; 20],
            nonce: 0,
            gas_price: U256::from(20_000_000_000u64),
            data: vec![],
            protection_type: super::super::ProtectionType::Private,
            timestamp: 1000,
        };

        let result = pool.submit_private(tx).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_bundle_submission() {
        let pool = PrivateTransactionPool::new();
        let searcher = [1u8; 20];
        
        pool.add_to_whitelist(searcher).await.unwrap();
        
        let bundle = PrivateBundle {
            id: H256::random(),
            transactions: vec![
                ProtectedTransaction {
                    tx_hash: H256::random(),
                    sender: searcher,
                    nonce: 0,
                    gas_price: U256::from(20_000_000_000u64),
                    data: vec![],
                    protection_type: super::super::ProtectionType::Private,
                    timestamp: 1000,
                },
            ],
            min_block: 100,
            max_block: 200,
            searcher,
            bid_amount: U256::from(10_000_000_000u64),
            revert_protection: true,
        };

        let result = pool.submit_bundle(bundle).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_bundle_ordering() {
        let pool = PrivateTransactionPool::new();
        
        for i in 0..5 {
            let searcher = [i as u8; 20];
            pool.add_to_whitelist(searcher).await.unwrap();
            
            let bundle = PrivateBundle {
                id: H256::random(),
                transactions: vec![],
                min_block: 0,
                max_block: 1000,
                searcher,
                bid_amount: U256::from((i + 1) as u64 * 1_000_000_000),
                revert_protection: false,
            };
            
            pool.submit_bundle(bundle).await.unwrap();
        }

        let bundles = pool.get_next_bundles(100).await;
        assert_eq!(bundles.len(), 5);
        
        for i in 0..bundles.len() - 1 {
            assert!(bundles[i].bid_amount >= bundles[i + 1].bid_amount);
        }
    }
}