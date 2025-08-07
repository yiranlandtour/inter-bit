use super::{
    BatchState, BatchStatus, ExecutionStep, FraudProof, Layer2Config, Layer2Error, Transaction,
};
use crate::state_machine::StateTransition;
use primitive_types::{H256, U256};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct OptimisticRollup {
    config: Layer2Config,
    pending_transactions: Arc<RwLock<VecDeque<Transaction>>>,
    batches: Arc<RwLock<HashMap<H256, Batch>>>,
    state_roots: Arc<RwLock<HashMap<u64, H256>>>,
    challenges: Arc<RwLock<HashMap<H256, Challenge>>>,
    sequencer: Arc<RwLock<Sequencer>>,
    da_layer: Arc<super::data_availability::DataAvailabilityLayer>,
    fraud_prover: Arc<FraudProver>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Batch {
    pub id: H256,
    pub transactions: Vec<Transaction>,
    pub state_root: H256,
    pub prev_state_root: H256,
    pub timestamp: u64,
    pub block_number: u64,
    pub proposer: [u8; 20],
    pub status: BatchState,
    pub challenge_deadline: u64,
}

#[derive(Debug, Clone)]
struct Challenge {
    pub batch_id: H256,
    pub challenger: [u8; 20],
    pub bond: U256,
    pub fraud_proof: FraudProof,
    pub status: ChallengeStatus,
    pub resolution_time: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
enum ChallengeStatus {
    Pending,
    Verified,
    Rejected,
    Expired,
}

struct Sequencer {
    address: [u8; 20],
    next_batch_id: u64,
    current_state_root: H256,
}

struct FraudProver {
    verification_key: Vec<u8>,
}

impl OptimisticRollup {
    pub fn new(
        config: Layer2Config,
        da_layer: Arc<super::data_availability::DataAvailabilityLayer>,
    ) -> Self {
        Self {
            config,
            pending_transactions: Arc::new(RwLock::new(VecDeque::new())),
            batches: Arc::new(RwLock::new(HashMap::new())),
            state_roots: Arc::new(RwLock::new(HashMap::new())),
            challenges: Arc::new(RwLock::new(HashMap::new())),
            sequencer: Arc::new(RwLock::new(Sequencer {
                address: [0u8; 20],
                next_batch_id: 1,
                current_state_root: H256::zero(),
            })),
            da_layer,
            fraud_prover: Arc::new(FraudProver {
                verification_key: vec![0u8; 32],
            }),
        }
    }

    pub async fn submit_transaction(&self, tx: Transaction) -> Result<H256, Layer2Error> {
        if !self.validate_transaction(&tx).await {
            return Err(Layer2Error::InvalidTransaction);
        }

        let tx_hash = self.hash_transaction(&tx);
        
        let mut pending = self.pending_transactions.write().await;
        pending.push_back(tx);

        if pending.len() >= self.config.batch_size {
            self.create_batch().await?;
        }

        Ok(tx_hash)
    }

    pub async fn create_batch(&self) -> Result<H256, Layer2Error> {
        let mut pending = self.pending_transactions.write().await;
        
        if pending.is_empty() {
            return Err(Layer2Error::InvalidTransaction);
        }

        let batch_size = std::cmp::min(pending.len(), self.config.batch_size);
        let mut batch_txs = Vec::with_capacity(batch_size);
        
        for _ in 0..batch_size {
            if let Some(tx) = pending.pop_front() {
                batch_txs.push(tx);
            }
        }

        drop(pending);

        let mut sequencer = self.sequencer.write().await;
        let batch_id = H256::from_low_u64_be(sequencer.next_batch_id);
        sequencer.next_batch_id += 1;

        let prev_state_root = sequencer.current_state_root;
        let new_state_root = self.compute_state_root(&batch_txs, prev_state_root).await;
        
        sequencer.current_state_root = new_state_root;

        let batch = Batch {
            id: batch_id,
            transactions: batch_txs.clone(),
            state_root: new_state_root,
            prev_state_root,
            timestamp: self.get_current_timestamp(),
            block_number: sequencer.next_batch_id - 1,
            proposer: sequencer.address,
            status: BatchState::Submitted,
            challenge_deadline: self.get_current_timestamp() + 
                (self.config.challenge_period_blocks * 12),
        };

        let batch_data = self.serialize_batch(&batch);
        self.da_layer.store_data(batch_id, batch_data).await?;

        let mut batches = self.batches.write().await;
        batches.insert(batch_id, batch.clone());

        let mut state_roots = self.state_roots.write().await;
        state_roots.insert(batch.block_number, new_state_root);

        self.submit_to_l1(batch_id, new_state_root).await?;

        Ok(batch_id)
    }

    pub async fn challenge_state_root(
        &self,
        batch_id: H256,
        challenger: [u8; 20],
        proof: FraudProof,
    ) -> Result<(), Layer2Error> {
        let batches = self.batches.read().await;
        let batch = batches.get(&batch_id).ok_or(Layer2Error::BatchNotFound)?;

        if batch.status != BatchState::Submitted {
            return Err(Layer2Error::InvalidProof);
        }

        let current_time = self.get_current_timestamp();
        if current_time > batch.challenge_deadline {
            return Err(Layer2Error::ChallengePeriodNotExpired);
        }

        if !self.verify_fraud_proof(&proof, batch).await {
            return Err(Layer2Error::InvalidProof);
        }

        let challenge = Challenge {
            batch_id,
            challenger,
            bond: self.config.min_bond_amount,
            fraud_proof: proof,
            status: ChallengeStatus::Pending,
            resolution_time: None,
        };

        let mut challenges = self.challenges.write().await;
        challenges.insert(batch_id, challenge);

        drop(batches);
        
        let mut batches = self.batches.write().await;
        if let Some(batch) = batches.get_mut(&batch_id) {
            batch.status = BatchState::Challenged;
        }

        self.execute_challenge_resolution(batch_id).await?;

        Ok(())
    }

    async fn execute_challenge_resolution(&self, batch_id: H256) -> Result<(), Layer2Error> {
        let challenges = self.challenges.read().await;
        let challenge = challenges.get(&batch_id).ok_or(Layer2Error::InvalidProof)?;

        let batches = self.batches.read().await;
        let batch = batches.get(&batch_id).ok_or(Layer2Error::BatchNotFound)?;

        let correct_state_root = self.recompute_state_root_with_witness(
            &batch.transactions,
            batch.prev_state_root,
            &challenge.fraud_proof.witness_data,
        ).await;

        drop(batches);
        drop(challenges);

        let mut batches = self.batches.write().await;
        let mut challenges = self.challenges.write().await;

        if let Some(batch) = batches.get_mut(&batch_id) {
            if correct_state_root != batch.state_root {
                batch.status = BatchState::Invalid;
                
                if let Some(challenge) = challenges.get_mut(&batch_id) {
                    challenge.status = ChallengeStatus::Verified;
                    challenge.resolution_time = Some(self.get_current_timestamp());
                }

                self.rollback_invalid_batch(batch_id).await?;
                self.slash_proposer(batch.proposer).await?;
                self.reward_challenger(challenge.challenger, self.config.min_bond_amount).await?;
            } else {
                batch.status = BatchState::Submitted;
                
                if let Some(challenge) = challenges.get_mut(&batch_id) {
                    challenge.status = ChallengeStatus::Rejected;
                    challenge.resolution_time = Some(self.get_current_timestamp());
                }

                self.slash_challenger(challenge.challenger).await?;
            }
        }

        Ok(())
    }

    pub async fn finalize_batch(&self, batch_id: H256) -> Result<(), Layer2Error> {
        let mut batches = self.batches.write().await;
        let batch = batches.get_mut(&batch_id).ok_or(Layer2Error::BatchNotFound)?;

        if batch.status != BatchState::Submitted {
            return Err(Layer2Error::InvalidProof);
        }

        let current_time = self.get_current_timestamp();
        if current_time < batch.challenge_deadline {
            return Err(Layer2Error::ChallengePeriodNotExpired);
        }

        batch.status = BatchState::Finalized;

        self.execute_withdrawals(batch_id).await?;

        Ok(())
    }

    pub async fn get_batch_status(&self, batch_id: H256) -> Option<BatchStatus> {
        let batches = self.batches.read().await;
        batches.get(&batch_id).map(|batch| BatchStatus {
            batch_id: batch.id,
            state: batch.status.clone(),
            num_transactions: batch.transactions.len(),
            state_root: batch.state_root,
            timestamp: batch.timestamp,
            proposer: batch.proposer,
            finalized: batch.status == BatchState::Finalized,
        })
    }

    pub async fn has_batch(&self, batch_id: H256) -> bool {
        let batches = self.batches.read().await;
        batches.contains_key(&batch_id)
    }

    async fn validate_transaction(&self, tx: &Transaction) -> bool {
        if tx.gas_limit == 0 || tx.gas_price == U256::zero() {
            return false;
        }

        true
    }

    async fn compute_state_root(&self, transactions: &[Transaction], prev_root: H256) -> H256 {
        use sha3::{Digest, Keccak256};
        
        let mut hasher = Keccak256::new();
        hasher.update(&prev_root.0);
        
        for tx in transactions {
            hasher.update(&tx.id.0);
            hasher.update(&tx.from);
            hasher.update(&tx.to);
            hasher.update(&tx.value.to_little_endian());
            hasher.update(&tx.data);
        }
        
        H256::from_slice(&hasher.finalize())
    }

    async fn recompute_state_root_with_witness(
        &self,
        transactions: &[Transaction],
        prev_root: H256,
        _witness_data: &[u8],
    ) -> H256 {
        self.compute_state_root(transactions, prev_root).await
    }

    async fn verify_fraud_proof(&self, proof: &FraudProof, batch: &Batch) -> bool {
        if proof.batch_id != batch.id {
            return false;
        }

        if proof.invalid_state_root != batch.state_root {
            return false;
        }

        let recomputed = self.verify_execution_trace(&proof.execution_trace).await;
        
        recomputed == proof.correct_state_root
    }

    async fn verify_execution_trace(&self, trace: &[ExecutionStep]) -> H256 {
        use sha3::{Digest, Keccak256};
        
        let mut hasher = Keccak256::new();
        
        for step in trace {
            hasher.update(&step.pc.to_le_bytes());
            hasher.update(&[step.opcode]);
            
            for value in &step.stack {
                hasher.update(&value.to_little_endian());
            }
            
            hasher.update(&step.memory);
        }
        
        H256::from_slice(&hasher.finalize())
    }

    fn hash_transaction(&self, tx: &Transaction) -> H256 {
        use sha3::{Digest, Keccak256};
        
        let mut hasher = Keccak256::new();
        hasher.update(&tx.from);
        hasher.update(&tx.to);
        hasher.update(&tx.value.to_little_endian());
        hasher.update(&tx.nonce.to_le_bytes());
        hasher.update(&tx.data);
        
        H256::from_slice(&hasher.finalize())
    }

    fn serialize_batch(&self, batch: &Batch) -> Vec<u8> {
        bincode::serialize(batch).unwrap_or_default()
    }

    async fn submit_to_l1(&self, _batch_id: H256, _state_root: H256) -> Result<(), Layer2Error> {
        Ok(())
    }

    async fn rollback_invalid_batch(&self, _batch_id: H256) -> Result<(), Layer2Error> {
        Ok(())
    }

    async fn slash_proposer(&self, _proposer: [u8; 20]) -> Result<(), Layer2Error> {
        Ok(())
    }

    async fn slash_challenger(&self, _challenger: [u8; 20]) -> Result<(), Layer2Error> {
        Ok(())
    }

    async fn reward_challenger(&self, _challenger: [u8; 20], _amount: U256) -> Result<(), Layer2Error> {
        Ok(())
    }

    async fn execute_withdrawals(&self, _batch_id: H256) -> Result<(), Layer2Error> {
        Ok(())
    }

    fn get_current_timestamp(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }
}

impl FraudProver {
    pub fn generate_proof(
        &self,
        _batch: &Batch,
        _invalid_tx_index: usize,
    ) -> Result<FraudProof, Layer2Error> {
        Ok(FraudProof {
            batch_id: H256::random(),
            invalid_state_root: H256::random(),
            correct_state_root: H256::random(),
            witness_data: vec![],
            execution_trace: vec![],
        })
    }

    pub fn verify_proof(&self, _proof: &FraudProof) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_optimistic_rollup_submission() {
        let config = Layer2Config {
            rollup_type: super::super::RollupType::Optimistic,
            batch_size: 10,
            batch_timeout_ms: 1000,
            challenge_period_blocks: 100,
            min_bond_amount: U256::from(1000u64),
            data_availability_config: super::super::DataAvailabilityConfig {
                storage_type: super::super::StorageType::OnChain,
                compression_enabled: true,
                erasure_coding_enabled: false,
                replication_factor: 3,
            },
        };

        let da_layer = Arc::new(super::super::data_availability::DataAvailabilityLayer::new(
            config.data_availability_config.clone(),
        ));

        let rollup = OptimisticRollup::new(config, da_layer);

        let tx = Transaction {
            id: H256::random(),
            from: [1u8; 20],
            to: [2u8; 20],
            value: U256::from(100u64),
            data: vec![],
            nonce: 0,
            gas_price: U256::from(20_000_000_000u64),
            gas_limit: 21000,
            signature: vec![0u8; 65],
            requires_instant_finality: false,
        };

        let result = rollup.submit_transaction(tx).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_batch_creation() {
        let config = Layer2Config {
            rollup_type: super::super::RollupType::Optimistic,
            batch_size: 2,
            batch_timeout_ms: 1000,
            challenge_period_blocks: 100,
            min_bond_amount: U256::from(1000u64),
            data_availability_config: super::super::DataAvailabilityConfig {
                storage_type: super::super::StorageType::OnChain,
                compression_enabled: true,
                erasure_coding_enabled: false,
                replication_factor: 3,
            },
        };

        let da_layer = Arc::new(super::super::data_availability::DataAvailabilityLayer::new(
            config.data_availability_config.clone(),
        ));

        let rollup = OptimisticRollup::new(config, da_layer);

        for i in 0..2 {
            let tx = Transaction {
                id: H256::random(),
                from: [1u8; 20],
                to: [2u8; 20],
                value: U256::from(100u64 + i),
                data: vec![],
                nonce: i,
                gas_price: U256::from(20_000_000_000u64),
                gas_limit: 21000,
                signature: vec![0u8; 65],
                requires_instant_finality: false,
            };

            let _ = rollup.submit_transaction(tx).await;
        }

        let batch_id = H256::from_low_u64_be(1);
        let status = rollup.get_batch_status(batch_id).await;
        assert!(status.is_some());
    }
}