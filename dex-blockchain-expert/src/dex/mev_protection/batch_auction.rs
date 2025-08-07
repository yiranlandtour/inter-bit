use super::{MevProtectionError, ProtectedTransaction};
use primitive_types::{H256, U256};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchOrder {
    pub tx: ProtectedTransaction,
    pub bid_price: U256,
    pub priority_fee: U256,
    pub submission_time: u64, // Changed from Instant to u64 timestamp
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Batch {
    pub id: H256,
    pub orders: Vec<BatchOrder>,
    pub start_time: u64, // Changed from Instant to u64 timestamp
    pub end_time: u64, // Changed from Instant to u64 timestamp
    pub status: BatchStatus,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum BatchStatus {
    Collecting,
    Sealed,
    Executing,
    Completed,
    Failed,
}

pub struct BatchAuctionManager {
    current_batch: Arc<RwLock<Option<Batch>>>,
    pending_orders: Arc<RwLock<VecDeque<BatchOrder>>>,
    executed_batches: Arc<RwLock<HashMap<H256, Batch>>>,
    batch_interval: Duration,
    min_batch_size: usize,
    max_batch_size: usize,
    fair_ordering_enabled: bool,
}

impl BatchAuctionManager {
    pub fn new(batch_interval: Duration) -> Self {
        Self {
            current_batch: Arc::new(RwLock::new(None)),
            pending_orders: Arc::new(RwLock::new(VecDeque::new())),
            executed_batches: Arc::new(RwLock::new(HashMap::new())),
            batch_interval,
            min_batch_size: 10,
            max_batch_size: 1000,
            fair_ordering_enabled: true,
        }
    }

    pub async fn submit_to_batch(
        &self,
        tx: ProtectedTransaction,
    ) -> Result<H256, MevProtectionError> {
        let order = BatchOrder {
            bid_price: tx.gas_price,
            priority_fee: self.calculate_priority_fee(&tx),
            submission_time: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64,
            tx,
        };

        let mut pending = self.pending_orders.write().await;
        pending.push_back(order.clone());

        let mut current = self.current_batch.write().await;
        if current.is_none() || self.should_seal_batch(&current).await {
            self.create_new_batch(&mut current, &mut pending).await?;
        }

        if let Some(batch) = current.as_mut() {
            if batch.orders.len() < self.max_batch_size {
                batch.orders.push(order.clone());
            }
        }

        Ok(order.tx.tx_hash)
    }

    pub async fn execute_batch(&self) -> Result<Vec<ProtectedTransaction>, MevProtectionError> {
        let mut current = self.current_batch.write().await;
        
        if let Some(mut batch) = current.take() {
            if batch.orders.len() < self.min_batch_size {
                return Err(MevProtectionError::BatchAuctionFailed(
                    "Batch size below minimum".to_string(),
                ));
            }

            batch.status = BatchStatus::Sealed;
            let ordered_txs = self.order_batch_fairly(&batch).await?;
            
            batch.status = BatchStatus::Executing;
            
            let results = self.execute_ordered_transactions(ordered_txs.clone()).await?;
            
            batch.status = BatchStatus::Completed;
            batch.end_time = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64;
            
            let mut executed = self.executed_batches.write().await;
            executed.insert(batch.id, batch);
            
            Ok(results)
        } else {
            Ok(Vec::new())
        }
    }

    async fn order_batch_fairly(
        &self,
        batch: &Batch,
    ) -> Result<Vec<ProtectedTransaction>, MevProtectionError> {
        if !self.fair_ordering_enabled {
            let mut orders = batch.orders.clone();
            orders.sort_by(|a, b| b.bid_price.cmp(&a.bid_price));
            return Ok(orders.into_iter().map(|o| o.tx).collect());
        }

        let mut price_groups: BTreeMap<U256, Vec<BatchOrder>> = BTreeMap::new();
        for order in &batch.orders {
            price_groups
                .entry(order.bid_price)
                .or_insert_with(Vec::new)
                .push(order.clone());
        }

        let mut fair_ordered = Vec::new();
        for (_, mut group) in price_groups.into_iter().rev() {
            group.sort_by_key(|o| o.submission_time);
            
            let group_hash = self.calculate_group_hash(&group);
            let shuffled = self.deterministic_shuffle(group, group_hash);
            
            fair_ordered.extend(shuffled);
        }

        Ok(fair_ordered.into_iter().map(|o| o.tx).collect())
    }

    async fn execute_ordered_transactions(
        &self,
        txs: Vec<ProtectedTransaction>,
    ) -> Result<Vec<ProtectedTransaction>, MevProtectionError> {
        let mut results = Vec::new();
        let mut state_snapshot = self.create_state_snapshot().await;

        for tx in txs {
            match self.simulate_transaction(&tx, &state_snapshot).await {
                Ok(success) => {
                    if success {
                        results.push(tx.clone());
                        self.update_state_snapshot(&mut state_snapshot, &tx).await;
                    }
                }
                Err(_) => continue,
            }
        }

        Ok(results)
    }

    async fn should_seal_batch(&self, current: &Option<Batch>) -> bool {
        if let Some(batch) = current {
            let current_time = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64;
            let elapsed_ns = current_time - batch.start_time;
            let elapsed_duration = std::time::Duration::from_nanos(elapsed_ns);
            elapsed_duration >= self.batch_interval || batch.orders.len() >= self.max_batch_size
        } else {
            false
        }
    }

    async fn create_new_batch(
        &self,
        current: &mut Option<Batch>,
        pending: &mut VecDeque<BatchOrder>,
    ) -> Result<(), MevProtectionError> {
        let batch_id = H256::random();
        let mut orders = Vec::new();

        while !pending.is_empty() && orders.len() < self.max_batch_size {
            if let Some(order) = pending.pop_front() {
                orders.push(order);
            }
        }

        *current = Some(Batch {
            id: batch_id,
            orders,
            start_time: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64,
            end_time: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64,
            status: BatchStatus::Collecting,
        });

        Ok(())
    }

    fn calculate_priority_fee(&self, tx: &ProtectedTransaction) -> U256 {
        let base_fee = U256::from(1_000_000_000u64);
        if tx.gas_price > base_fee {
            tx.gas_price - base_fee
        } else {
            U256::zero()
        }
    }

    fn calculate_group_hash(&self, group: &[BatchOrder]) -> H256 {
        use sha3::{Digest, Keccak256};
        let mut hasher = Keccak256::new();
        
        for order in group {
            hasher.update(&order.tx.tx_hash.0);
        }
        
        H256::from_slice(&hasher.finalize())
    }

    fn deterministic_shuffle(&self, mut group: Vec<BatchOrder>, seed: H256) -> Vec<BatchOrder> {
        use rand::{rngs::StdRng, seq::SliceRandom, SeedableRng};
        
        let mut seed_bytes = [0u8; 32];
        seed_bytes.copy_from_slice(&seed.0);
        let mut rng = StdRng::from_seed(seed_bytes);
        
        group.shuffle(&mut rng);
        group
    }

    async fn create_state_snapshot(&self) -> StateSnapshot {
        StateSnapshot {
            balances: HashMap::new(),
            nonces: HashMap::new(),
            storage: HashMap::new(),
        }
    }

    async fn simulate_transaction(
        &self,
        _tx: &ProtectedTransaction,
        _snapshot: &StateSnapshot,
    ) -> Result<bool, MevProtectionError> {
        Ok(true)
    }

    async fn update_state_snapshot(&self, snapshot: &mut StateSnapshot, tx: &ProtectedTransaction) {
        snapshot.nonces.insert(tx.sender, tx.nonce + 1);
    }

    pub async fn get_batch_info(&self, batch_id: H256) -> Option<BatchInfo> {
        let executed = self.executed_batches.read().await;
        executed.get(&batch_id).map(|batch| BatchInfo {
            id: batch.id,
            size: batch.orders.len(),
            total_value: batch
                .orders
                .iter()
                .fold(U256::zero(), |acc, o| acc + o.bid_price),
            execution_time: (batch.end_time - batch.start_time) / 1_000_000, // Convert nanoseconds to milliseconds
            status: batch.status.clone(),
        })
    }

    pub async fn get_current_batch_status(&self) -> Option<BatchStatus> {
        let current = self.current_batch.read().await;
        current.as_ref().map(|b| b.status.clone())
    }
}

#[derive(Debug)]
struct StateSnapshot {
    balances: HashMap<[u8; 20], U256>,
    nonces: HashMap<[u8; 20], u64>,
    storage: HashMap<H256, Vec<u8>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchInfo {
    pub id: H256,
    pub size: usize,
    pub total_value: U256,
    pub execution_time: u64, // Changed from Duration to u64 milliseconds
    pub status: BatchStatus,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_batch_auction_submission() {
        let manager = BatchAuctionManager::new(Duration::from_millis(100));
        
        let tx = ProtectedTransaction {
            tx_hash: H256::random(),
            sender: [1u8; 20],
            nonce: 0,
            gas_price: U256::from(20_000_000_000u64),
            data: vec![],
            protection_type: super::super::ProtectionType::BatchAuction,
            timestamp: 1000,
        };

        let result = manager.submit_to_batch(tx).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_batch_fair_ordering() {
        let manager = BatchAuctionManager::new(Duration::from_millis(100));
        
        for i in 0..20 {
            let tx = ProtectedTransaction {
                tx_hash: H256::random(),
                sender: [i as u8; 20],
                nonce: 0,
                gas_price: U256::from(20_000_000_000u64 + (i as u64 * 1_000_000_000)),
                data: vec![],
                protection_type: super::super::ProtectionType::BatchAuction,
                timestamp: 1000 + i as u64,
            };
            let _ = manager.submit_to_batch(tx).await;
        }

        tokio::time::sleep(Duration::from_millis(150)).await;
        
        let result = manager.execute_batch().await;
        assert!(result.is_ok());
        
        let executed_txs = result.unwrap();
        assert!(!executed_txs.is_empty());
    }
}