use super::{CrossChainError, RelayerConfig};
use primitive_types::H256;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{interval, Duration};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayMessage {
    pub id: H256,
    pub source_chain: u64,
    pub destination_chain: u64,
    pub nonce: u64,
    pub payload: Vec<u8>,
    pub timestamp: u64,
    pub confirmations: u64,
    pub status: RelayStatus,
    pub retry_count: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RelayStatus {
    Pending,
    Confirming,
    Ready,
    Relayed,
    Failed,
}

pub struct Relayer {
    config: RelayerConfig,
    message_queue: Arc<RwLock<VecDeque<RelayMessage>>>,
    pending_messages: Arc<RwLock<HashMap<H256, RelayMessage>>>,
    relayed_messages: Arc<RwLock<HashMap<H256, RelayMessage>>>,
    chain_nonces: Arc<RwLock<HashMap<u64, u64>>>,
    active: Arc<RwLock<bool>>,
}

impl Relayer {
    pub fn new(config: RelayerConfig) -> Self {
        Self {
            config,
            message_queue: Arc::new(RwLock::new(VecDeque::new())),
            pending_messages: Arc::new(RwLock::new(HashMap::new())),
            relayed_messages: Arc::new(RwLock::new(HashMap::new())),
            chain_nonces: Arc::new(RwLock::new(HashMap::new())),
            active: Arc::new(RwLock::new(false)),
        }
    }

    pub async fn start(&self) {
        let mut active = self.active.write().await;
        *active = true;
        drop(active);

        let relayer = self.clone();
        tokio::spawn(async move {
            relayer.relay_loop().await;
        });
    }

    pub async fn stop(&self) {
        let mut active = self.active.write().await;
        *active = false;
    }

    pub async fn submit_transfer(&self, transfer_id: H256) -> Result<(), CrossChainError> {
        let message = RelayMessage {
            id: transfer_id,
            source_chain: 1,
            destination_chain: 2,
            nonce: self.get_next_nonce(1).await,
            payload: vec![],
            timestamp: self.get_current_timestamp(),
            confirmations: 0,
            status: RelayStatus::Pending,
            retry_count: 0,
        };

        let mut queue = self.message_queue.write().await;
        queue.push_back(message.clone());

        let mut pending = self.pending_messages.write().await;
        pending.insert(transfer_id, message);

        Ok(())
    }

    async fn relay_loop(&self) {
        let mut ticker = interval(Duration::from_millis(self.config.relay_interval_ms));

        loop {
            ticker.tick().await;

            let active = self.active.read().await;
            if !*active {
                break;
            }
            drop(active);

            if let Err(e) = self.process_pending_messages().await {
                eprintln!("Relay error: {:?}", e);
            }

            if let Err(e) = self.relay_ready_messages().await {
                eprintln!("Relay error: {:?}", e);
            }
        }
    }

    async fn process_pending_messages(&self) -> Result<(), CrossChainError> {
        let mut pending = self.pending_messages.write().await;
        let mut updated_messages = Vec::new();

        for (id, message) in pending.iter_mut() {
            if message.status == RelayStatus::Pending {
                message.confirmations += 1;
                
                if message.confirmations >= self.config.min_confirmations {
                    message.status = RelayStatus::Ready;
                    updated_messages.push((*id, message.clone()));
                }
            }
        }

        for (id, message) in updated_messages {
            if message.status == RelayStatus::Ready {
                let mut queue = self.message_queue.write().await;
                queue.push_back(message);
            }
        }

        Ok(())
    }

    async fn relay_ready_messages(&self) -> Result<(), CrossChainError> {
        let mut queue = self.message_queue.write().await;
        let batch_size = std::cmp::min(self.config.batch_size, queue.len());
        
        let mut batch = Vec::new();
        for _ in 0..batch_size {
            if let Some(message) = queue.pop_front() {
                if message.status == RelayStatus::Ready {
                    batch.push(message);
                } else {
                    queue.push_back(message);
                }
            }
        }
        drop(queue);

        for mut message in batch {
            match self.relay_message(&message).await {
                Ok(()) => {
                    message.status = RelayStatus::Relayed;
                    let mut relayed = self.relayed_messages.write().await;
                    relayed.insert(message.id, message.clone());
                    
                    let mut pending = self.pending_messages.write().await;
                    pending.remove(&message.id);
                }
                Err(e) => {
                    message.retry_count += 1;
                    
                    if message.retry_count >= self.config.max_retry_attempts {
                        message.status = RelayStatus::Failed;
                        eprintln!("Message {} failed after {} retries: {:?}", 
                            message.id, message.retry_count, e);
                    } else {
                        let mut queue = self.message_queue.write().await;
                        queue.push_back(message);
                    }
                }
            }
        }

        Ok(())
    }

    async fn relay_message(&self, message: &RelayMessage) -> Result<(), CrossChainError> {
        tokio::time::sleep(Duration::from_millis(10)).await;
        Ok(())
    }

    async fn get_next_nonce(&self, chain_id: u64) -> u64 {
        let mut nonces = self.chain_nonces.write().await;
        let nonce = nonces.entry(chain_id).or_insert(0);
        *nonce += 1;
        *nonce
    }

    fn get_current_timestamp(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    pub async fn get_message_status(&self, message_id: H256) -> Option<RelayStatus> {
        if let Some(message) = self.pending_messages.read().await.get(&message_id) {
            return Some(message.status.clone());
        }
        
        if let Some(message) = self.relayed_messages.read().await.get(&message_id) {
            return Some(message.status.clone());
        }
        
        None
    }

    pub async fn get_stats(&self) -> RelayerStats {
        let pending = self.pending_messages.read().await;
        let relayed = self.relayed_messages.read().await;
        let queue = self.message_queue.read().await;
        
        RelayerStats {
            pending_count: pending.len(),
            relayed_count: relayed.len(),
            queue_size: queue.len(),
            failed_count: pending.values().filter(|m| m.status == RelayStatus::Failed).count(),
        }
    }
}

impl Clone for Relayer {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            message_queue: Arc::clone(&self.message_queue),
            pending_messages: Arc::clone(&self.pending_messages),
            relayed_messages: Arc::clone(&self.relayed_messages),
            chain_nonces: Arc::clone(&self.chain_nonces),
            active: Arc::clone(&self.active),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayerStats {
    pub pending_count: usize,
    pub relayed_count: usize,
    pub queue_size: usize,
    pub failed_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_relayer_submit() {
        let config = RelayerConfig {
            min_confirmations: 3,
            batch_size: 10,
            relay_interval_ms: 100,
            max_retry_attempts: 3,
        };

        let relayer = Relayer::new(config);
        let transfer_id = H256::random();
        
        let result = relayer.submit_transfer(transfer_id).await;
        assert!(result.is_ok());
        
        let status = relayer.get_message_status(transfer_id).await;
        assert_eq!(status, Some(RelayStatus::Pending));
    }

    #[tokio::test]
    async fn test_relayer_processing() {
        let config = RelayerConfig {
            min_confirmations: 2,
            batch_size: 5,
            relay_interval_ms: 50,
            max_retry_attempts: 3,
        };

        let relayer = Relayer::new(config);
        
        for _ in 0..5 {
            let transfer_id = H256::random();
            relayer.submit_transfer(transfer_id).await.unwrap();
        }
        
        relayer.start().await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        
        let stats = relayer.get_stats().await;
        assert!(stats.pending_count > 0);
        
        relayer.stop().await;
    }
}