pub mod batch_auction;
pub mod threshold_encryption;
pub mod private_pool;
pub mod commit_reveal;

use primitive_types::{H256, U256};
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MevProtectionConfig {
    pub batch_auction_enabled: bool,
    pub batch_interval: Duration,
    pub threshold_encryption_enabled: bool,
    pub encryption_threshold: usize,
    pub private_pool_enabled: bool,
    pub commit_reveal_enabled: bool,
    pub reveal_timeout: Duration,
}

impl Default for MevProtectionConfig {
    fn default() -> Self {
        Self {
            batch_auction_enabled: true,
            batch_interval: Duration::from_millis(100),
            threshold_encryption_enabled: true,
            encryption_threshold: 3,
            private_pool_enabled: true,
            commit_reveal_enabled: true,
            reveal_timeout: Duration::from_secs(10),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtectedTransaction {
    pub tx_hash: H256,
    pub sender: [u8; 20],
    pub nonce: u64,
    pub gas_price: U256,
    pub data: Vec<u8>,
    pub protection_type: ProtectionType,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProtectionType {
    BatchAuction,
    ThresholdEncrypted,
    Private,
    CommitReveal,
    None,
}

#[derive(Debug)]
pub enum MevProtectionError {
    BatchAuctionFailed(String),
    EncryptionFailed(String),
    DecryptionFailed(String),
    ThresholdNotMet,
    RevealTimeout,
    InvalidCommitment,
    PrivatePoolAccessDenied,
}

pub struct MevProtectionSystem {
    config: MevProtectionConfig,
    batch_auction: batch_auction::BatchAuctionManager,
    threshold_encryption: threshold_encryption::ThresholdEncryptionSystem,
    private_pool: private_pool::PrivateTransactionPool,
    commit_reveal: commit_reveal::CommitRevealScheme,
}

impl MevProtectionSystem {
    pub fn new(config: MevProtectionConfig) -> Self {
        Self {
            config: config.clone(),
            batch_auction: batch_auction::BatchAuctionManager::new(config.batch_interval),
            threshold_encryption: threshold_encryption::ThresholdEncryptionSystem::new(
                config.encryption_threshold,
            ),
            private_pool: private_pool::PrivateTransactionPool::new(),
            commit_reveal: commit_reveal::CommitRevealScheme::new(config.reveal_timeout),
        }
    }

    pub async fn submit_transaction(
        &self,
        tx: ProtectedTransaction,
    ) -> Result<H256, MevProtectionError> {
        match tx.protection_type {
            ProtectionType::BatchAuction => {
                self.batch_auction.submit_to_batch(tx).await
            }
            ProtectionType::ThresholdEncrypted => {
                self.threshold_encryption.submit_encrypted(tx).await
            }
            ProtectionType::Private => {
                self.private_pool.submit_private(tx).await
            }
            ProtectionType::CommitReveal => {
                self.commit_reveal.submit_commitment(tx).await
            }
            ProtectionType::None => {
                Ok(tx.tx_hash)
            }
        }
    }

    pub async fn process_batch(&self) -> Result<Vec<ProtectedTransaction>, MevProtectionError> {
        if self.config.batch_auction_enabled {
            self.batch_auction.execute_batch().await
        } else {
            Ok(Vec::new())
        }
    }

    pub async fn decrypt_transactions(
        &self,
        key_shares: Vec<Vec<u8>>,
    ) -> Result<Vec<ProtectedTransaction>, MevProtectionError> {
        if self.config.threshold_encryption_enabled {
            self.threshold_encryption.decrypt_batch(key_shares).await
        } else {
            Ok(Vec::new())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mev_protection_system() {
        let config = MevProtectionConfig::default();
        let system = MevProtectionSystem::new(config);

        let tx = ProtectedTransaction {
            tx_hash: H256::random(),
            sender: [0u8; 20],
            nonce: 1,
            gas_price: U256::from(20_000_000_000u64),
            data: vec![1, 2, 3],
            protection_type: ProtectionType::BatchAuction,
            timestamp: 1000,
        };

        let result = system.submit_transaction(tx).await;
        assert!(result.is_ok());
    }
}