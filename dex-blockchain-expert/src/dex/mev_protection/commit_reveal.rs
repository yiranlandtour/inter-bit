use super::{MevProtectionError, ProtectedTransaction};
use primitive_types::H256;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Commitment {
    pub commitment_hash: H256,
    pub sender: [u8; 20],
    pub timestamp: u64,
    pub reveal_deadline: u64,
    pub revealed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevealedTransaction {
    pub commitment_hash: H256,
    pub tx: ProtectedTransaction,
    pub nonce_value: H256,
    pub reveal_timestamp: u64,
}

pub struct CommitRevealScheme {
    commitments: Arc<RwLock<HashMap<H256, Commitment>>>,
    revealed_txs: Arc<RwLock<HashMap<H256, RevealedTransaction>>>,
    pending_reveals: Arc<RwLock<Vec<RevealedTransaction>>>,
    reveal_timeout: Duration,
    min_commit_period: Duration,
}

impl CommitRevealScheme {
    pub fn new(reveal_timeout: Duration) -> Self {
        Self {
            commitments: Arc::new(RwLock::new(HashMap::new())),
            revealed_txs: Arc::new(RwLock::new(HashMap::new())),
            pending_reveals: Arc::new(RwLock::new(Vec::new())),
            reveal_timeout,
            min_commit_period: Duration::from_secs(1),
        }
    }

    pub async fn submit_commitment(
        &self,
        tx: ProtectedTransaction,
    ) -> Result<H256, MevProtectionError> {
        let nonce_value = H256::random();
        let commitment_hash = self.compute_commitment(&tx, nonce_value);
        
        let commitment = Commitment {
            commitment_hash,
            sender: tx.sender,
            timestamp: self.get_current_timestamp(),
            reveal_deadline: self.get_current_timestamp() + self.reveal_timeout.as_secs(),
            revealed: false,
        };

        let mut commitments = self.commitments.write().await;
        commitments.insert(commitment_hash, commitment.clone());

        self.store_commitment_data(commitment_hash, tx, nonce_value).await;

        Ok(commitment_hash)
    }

    pub async fn reveal_transaction(
        &self,
        commitment_hash: H256,
        tx: ProtectedTransaction,
        nonce_value: H256,
    ) -> Result<(), MevProtectionError> {
        let mut commitments = self.commitments.write().await;
        
        let commitment = commitments
            .get_mut(&commitment_hash)
            .ok_or(MevProtectionError::InvalidCommitment)?;

        if commitment.revealed {
            return Err(MevProtectionError::InvalidCommitment);
        }

        let current_time = self.get_current_timestamp();
        if current_time > commitment.reveal_deadline {
            return Err(MevProtectionError::RevealTimeout);
        }

        if current_time < commitment.timestamp + self.min_commit_period.as_secs() {
            return Err(MevProtectionError::InvalidCommitment);
        }

        let computed_hash = self.compute_commitment(&tx, nonce_value);
        if computed_hash != commitment_hash {
            return Err(MevProtectionError::InvalidCommitment);
        }

        commitment.revealed = true;

        let revealed = RevealedTransaction {
            commitment_hash,
            tx: tx.clone(),
            nonce_value,
            reveal_timestamp: current_time,
        };

        let mut revealed_txs = self.revealed_txs.write().await;
        revealed_txs.insert(commitment_hash, revealed.clone());

        let mut pending = self.pending_reveals.write().await;
        pending.push(revealed);

        Ok(())
    }

    pub async fn get_ready_transactions(&self) -> Vec<ProtectedTransaction> {
        let mut pending = self.pending_reveals.write().await;
        let current_time = self.get_current_timestamp();
        
        let mut ready = Vec::new();
        let mut remaining = Vec::new();

        for revealed in pending.drain(..) {
            if current_time >= revealed.reveal_timestamp + 1 {
                ready.push(revealed.tx);
            } else {
                remaining.push(revealed);
            }
        }

        *pending = remaining;
        
        ready.sort_by_key(|tx| (tx.gas_price, tx.nonce));
        ready.reverse();
        
        ready
    }

    pub async fn slash_expired_commitments(&self) -> Vec<[u8; 20]> {
        let mut commitments = self.commitments.write().await;
        let current_time = self.get_current_timestamp();
        let mut slashed_addresses = Vec::new();

        commitments.retain(|_hash, commitment| {
            if !commitment.revealed && current_time > commitment.reveal_deadline {
                slashed_addresses.push(commitment.sender);
                false
            } else {
                true
            }
        });

        slashed_addresses
    }

    pub async fn verify_commitment(
        &self,
        commitment_hash: H256,
        tx: &ProtectedTransaction,
        nonce_value: H256,
    ) -> bool {
        let computed = self.compute_commitment(tx, nonce_value);
        computed == commitment_hash
    }

    fn compute_commitment(&self, tx: &ProtectedTransaction, nonce_value: H256) -> H256 {
        use sha3::{Digest, Keccak256};
        
        let mut hasher = Keccak256::new();
        hasher.update(&tx.tx_hash.0);
        hasher.update(&tx.sender);
        hasher.update(&tx.nonce.to_le_bytes());
        hasher.update(&tx.gas_price.to_little_endian().as_slice()[..32]);
        hasher.update(&tx.data);
        hasher.update(&nonce_value.0);
        
        H256::from_slice(&hasher.finalize())
    }

    async fn store_commitment_data(
        &self,
        _commitment_hash: H256,
        _tx: ProtectedTransaction,
        _nonce_value: H256,
    ) {
    }

    fn get_current_timestamp(&self) -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    pub async fn get_commitment_status(&self, commitment_hash: H256) -> Option<CommitmentStatus> {
        let commitments = self.commitments.read().await;
        let revealed_txs = self.revealed_txs.read().await;
        
        commitments.get(&commitment_hash).map(|c| {
            let is_revealed = revealed_txs.contains_key(&commitment_hash);
            let current_time = self.get_current_timestamp();
            
            CommitmentStatus {
                commitment_hash,
                sender: c.sender,
                committed_at: c.timestamp,
                reveal_deadline: c.reveal_deadline,
                is_revealed,
                is_expired: !is_revealed && current_time > c.reveal_deadline,
                time_remaining: if current_time < c.reveal_deadline {
                    Some(c.reveal_deadline - current_time)
                } else {
                    None
                },
            }
        })
    }

    pub async fn batch_reveal(
        &self,
        reveals: Vec<(H256, ProtectedTransaction, H256)>,
    ) -> Result<Vec<H256>, MevProtectionError> {
        let mut successful = Vec::new();
        
        for (commitment_hash, tx, nonce) in reveals {
            match self.reveal_transaction(commitment_hash, tx, nonce).await {
                Ok(()) => successful.push(commitment_hash),
                Err(_) => continue,
            }
        }
        
        Ok(successful)
    }

    pub async fn get_statistics(&self) -> CommitRevealStats {
        let commitments = self.commitments.read().await;
        let revealed = self.revealed_txs.read().await;
        let pending = self.pending_reveals.read().await;
        
        let current_time = self.get_current_timestamp();
        let expired = commitments.values().filter(|c| {
            !c.revealed && current_time > c.reveal_deadline
        }).count();
        
        CommitRevealStats {
            total_commitments: commitments.len(),
            revealed_count: revealed.len(),
            pending_reveals: pending.len(),
            expired_count: expired,
            average_reveal_time: self.calculate_average_reveal_time(&commitments, &revealed),
        }
    }

    fn calculate_average_reveal_time(
        &self,
        commitments: &HashMap<H256, Commitment>,
        revealed: &HashMap<H256, RevealedTransaction>,
    ) -> Option<u64> {
        let mut total_time = 0u64;
        let mut count = 0u64;
        
        for (hash, reveal) in revealed.iter() {
            if let Some(commitment) = commitments.get(hash) {
                total_time += reveal.reveal_timestamp - commitment.timestamp;
                count += 1;
            }
        }
        
        if count > 0 {
            Some(total_time / count)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitmentStatus {
    pub commitment_hash: H256,
    pub sender: [u8; 20],
    pub committed_at: u64,
    pub reveal_deadline: u64,
    pub is_revealed: bool,
    pub is_expired: bool,
    pub time_remaining: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitRevealStats {
    pub total_commitments: usize,
    pub revealed_count: usize,
    pub pending_reveals: usize,
    pub expired_count: usize,
    pub average_reveal_time: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_commit_reveal_flow() {
        let scheme = CommitRevealScheme::new(Duration::from_secs(10));
        
        let tx = ProtectedTransaction {
            tx_hash: H256::random(),
            sender: [1u8; 20],
            nonce: 1,
            gas_price: primitive_types::U256::from(20_000_000_000u64),
            data: vec![1, 2, 3],
            protection_type: super::super::ProtectionType::CommitReveal,
            timestamp: 1000,
        };

        let commitment_hash = scheme.submit_commitment(tx.clone()).await.unwrap();
        
        tokio::time::sleep(Duration::from_secs(2)).await;
        
        let nonce = H256::random();
        let result = scheme.reveal_transaction(
            commitment_hash,
            tx,
            nonce,
        ).await;
        
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_commitment_verification() {
        let scheme = CommitRevealScheme::new(Duration::from_secs(10));
        
        let tx = ProtectedTransaction {
            tx_hash: H256::random(),
            sender: [1u8; 20],
            nonce: 1,
            gas_price: primitive_types::U256::from(20_000_000_000u64),
            data: vec![1, 2, 3],
            protection_type: super::super::ProtectionType::CommitReveal,
            timestamp: 1000,
        };

        let nonce = H256::random();
        let commitment = scheme.compute_commitment(&tx, nonce);
        
        let verified = scheme.verify_commitment(commitment, &tx, nonce).await;
        assert!(verified);
        
        let wrong_nonce = H256::random();
        let not_verified = scheme.verify_commitment(commitment, &tx, wrong_nonce).await;
        assert!(!not_verified);
    }

    #[tokio::test]
    async fn test_expired_commitment_slashing() {
        let scheme = CommitRevealScheme::new(Duration::from_millis(100));
        
        let tx = ProtectedTransaction {
            tx_hash: H256::random(),
            sender: [1u8; 20],
            nonce: 1,
            gas_price: primitive_types::U256::from(20_000_000_000u64),
            data: vec![],
            protection_type: super::super::ProtectionType::CommitReveal,
            timestamp: 1000,
        };

        let _ = scheme.submit_commitment(tx).await.unwrap();
        
        tokio::time::sleep(Duration::from_millis(200)).await;
        
        let slashed = scheme.slash_expired_commitments().await;
        assert_eq!(slashed.len(), 1);
        assert_eq!(slashed[0], [1u8; 20]);
    }
}