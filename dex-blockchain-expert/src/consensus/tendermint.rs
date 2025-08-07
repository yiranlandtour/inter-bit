use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, RwLock};
use tokio::time::{interval, timeout};

use super::types::*;
use super::validator::Validator;

const BLOCK_TIME_MS: u64 = 100; // 100ms block time for <200ms latency
const TIMEOUT_PROPOSE: Duration = Duration::from_millis(50);
const TIMEOUT_PREVOTE: Duration = Duration::from_millis(30);
const TIMEOUT_PRECOMMIT: Duration = Duration::from_millis(30);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    pub height: u64,
    pub timestamp: u64,
    pub previous_hash: [u8; 32],
    pub state_root: [u8; 32],
    pub transactions: Vec<Transaction>,
    pub proposer: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transaction {
    pub nonce: u64,
    pub from: [u8; 20],
    pub to: Option<[u8; 20]>,
    pub value: u128,
    pub data: Vec<u8>,
    pub gas_limit: u64,
    pub gas_price: u128,
    pub signature: Vec<u8>,
}

impl Block {
    pub fn hash(&self) -> [u8; 32] {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(&self.height.to_be_bytes());
        hasher.update(&self.timestamp.to_be_bytes());
        hasher.update(&self.previous_hash);
        hasher.update(&self.state_root);
        hasher.update(&self.proposer);
        
        for tx in &self.transactions {
            hasher.update(&tx.nonce.to_be_bytes());
            hasher.update(&tx.from);
            if let Some(to) = tx.to {
                hasher.update(&to);
            }
            hasher.update(&tx.value.to_be_bytes());
            hasher.update(&tx.data);
            hasher.update(&tx.gas_limit.to_be_bytes());
            hasher.update(&tx.gas_price.to_be_bytes());
            hasher.update(&tx.signature);
        }
        
        let result = hasher.finalize();
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&result);
        hash
    }
}

impl Transaction {
    pub fn hash(&self) -> [u8; 32] {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(&self.nonce.to_be_bytes());
        hasher.update(&self.from);
        if let Some(to) = self.to {
            hasher.update(&to);
        }
        hasher.update(&self.value.to_be_bytes());
        hasher.update(&self.data);
        hasher.update(&self.gas_limit.to_be_bytes());
        hasher.update(&self.gas_price.to_be_bytes());
        
        let result = hasher.finalize();
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&result);
        hash
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConsensusState {
    NewHeight,
    Propose,
    Prevote,
    Precommit,
    Commit,
}

#[derive(Debug, Clone)]
pub struct Vote {
    pub height: u64,
    pub round: u32,
    pub vote_type: VoteType,
    pub block_hash: Option<[u8; 32]>,
    pub validator: Vec<u8>,
    pub signature: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum VoteType {
    Prevote,
    Precommit,
}

pub struct TendermintConsensus {
    height: Arc<RwLock<u64>>,
    round: Arc<RwLock<u32>>,
    state: Arc<RwLock<ConsensusState>>,
    validators: Arc<RwLock<Vec<Validator>>>,
    votes: Arc<RwLock<HashMap<(u64, u32, VoteType), Vec<Vote>>>>,
    locked_block: Arc<RwLock<Option<Block>>>,
    locked_round: Arc<RwLock<Option<u32>>>,
    valid_block: Arc<RwLock<Option<Block>>>,
    valid_round: Arc<RwLock<Option<u32>>>,
    block_tx: mpsc::Sender<Block>,
    vote_rx: Arc<RwLock<mpsc::Receiver<Vote>>>,
    vote_tx: mpsc::Sender<Vote>,
}

impl TendermintConsensus {
    pub fn new(validators: Vec<Validator>) -> (Self, mpsc::Receiver<Block>) {
        let (block_tx, block_rx) = mpsc::channel(100);
        let (vote_tx, vote_rx) = mpsc::channel(1000);
        
        (
            Self {
                height: Arc::new(RwLock::new(0)),
                round: Arc::new(RwLock::new(0)),
                state: Arc::new(RwLock::new(ConsensusState::NewHeight)),
                validators: Arc::new(RwLock::new(validators)),
                votes: Arc::new(RwLock::new(HashMap::new())),
                locked_block: Arc::new(RwLock::new(None)),
                locked_round: Arc::new(RwLock::new(None)),
                valid_block: Arc::new(RwLock::new(None)),
                valid_round: Arc::new(RwLock::new(None)),
                block_tx,
                vote_rx: Arc::new(RwLock::new(vote_rx)),
                vote_tx,
            },
            block_rx,
        )
    }

    pub async fn start(&self) {
        let mut interval = interval(Duration::from_millis(BLOCK_TIME_MS));
        
        loop {
            interval.tick().await;
            
            let state = self.state.read().await.clone();
            
            match state {
                ConsensusState::NewHeight => {
                    self.start_new_height().await;
                }
                ConsensusState::Propose => {
                    self.handle_propose().await;
                }
                ConsensusState::Prevote => {
                    self.handle_prevote().await;
                }
                ConsensusState::Precommit => {
                    self.handle_precommit().await;
                }
                ConsensusState::Commit => {
                    self.handle_commit().await;
                }
            }
            
            self.process_votes().await;
        }
    }

    async fn start_new_height(&self) {
        let mut height = self.height.write().await;
        *height += 1;
        
        let mut round = self.round.write().await;
        *round = 0;
        
        let mut state = self.state.write().await;
        *state = ConsensusState::Propose;
        
        tracing::info!("Starting new height: {}", *height);
    }

    async fn handle_propose(&self) {
        let height = *self.height.read().await;
        let round = *self.round.read().await;
        
        if self.is_proposer(height, round).await {
            let block = self.create_block().await;
            
            let vote = Vote {
                height,
                round,
                vote_type: VoteType::Prevote,
                block_hash: Some(self.hash_block(&block)),
                validator: vec![1, 2, 3], // Placeholder validator ID
                signature: vec![],
            };
            
            let _ = self.vote_tx.send(vote).await;
            
            let mut valid_block = self.valid_block.write().await;
            *valid_block = Some(block);
        }
        
        timeout(TIMEOUT_PROPOSE, async {
            let mut state = self.state.write().await;
            *state = ConsensusState::Prevote;
        })
        .await
        .ok();
    }

    async fn handle_prevote(&self) {
        let height = *self.height.read().await;
        let round = *self.round.read().await;
        
        if self.has_two_thirds_prevotes(height, round).await {
            let mut state = self.state.write().await;
            *state = ConsensusState::Precommit;
        } else {
            timeout(TIMEOUT_PREVOTE, async {
                let mut state = self.state.write().await;
                *state = ConsensusState::Precommit;
            })
            .await
            .ok();
        }
    }

    async fn handle_precommit(&self) {
        let height = *self.height.read().await;
        let round = *self.round.read().await;
        
        if self.has_two_thirds_precommits(height, round).await {
            let mut state = self.state.write().await;
            *state = ConsensusState::Commit;
        } else {
            timeout(TIMEOUT_PRECOMMIT, async {
                self.start_new_round().await;
            })
            .await
            .ok();
        }
    }

    async fn handle_commit(&self) {
        if let Some(block) = self.valid_block.read().await.clone() {
            let _ = self.block_tx.send(block).await;
            
            let mut state = self.state.write().await;
            *state = ConsensusState::NewHeight;
            
            self.reset_round_state().await;
        }
    }

    async fn process_votes(&self) {
        let mut vote_rx = self.vote_rx.write().await;
        
        while let Ok(vote) = vote_rx.try_recv() {
            let mut votes = self.votes.write().await;
            let key = (vote.height, vote.round, vote.vote_type.clone());
            votes.entry(key).or_insert_with(Vec::new).push(vote);
        }
    }

    async fn is_proposer(&self, height: u64, round: u32) -> bool {
        let validators = self.validators.read().await;
        let proposer_index = ((height + round as u64) % validators.len() as u64) as usize;
        // Check if current node is the proposer
        true // Simplified for demo
    }

    async fn create_block(&self) -> Block {
        Block {
            height: *self.height.read().await,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
            previous_hash: [0; 32],
            state_root: [0; 32],
            transactions: vec![],
            proposer: vec![1, 2, 3],
        }
    }

    fn hash_block(&self, block: &Block) -> [u8; 32] {
        use sha2::{Digest, Sha256};
        let data = bincode::serialize(block).unwrap();
        let hash = Sha256::digest(&data);
        let mut result = [0; 32];
        result.copy_from_slice(&hash);
        result
    }

    async fn has_two_thirds_prevotes(&self, height: u64, round: u32) -> bool {
        let votes = self.votes.read().await;
        let validators = self.validators.read().await;
        
        if let Some(prevotes) = votes.get(&(height, round, VoteType::Prevote)) {
            prevotes.len() >= (validators.len() * 2 / 3 + 1)
        } else {
            false
        }
    }

    async fn has_two_thirds_precommits(&self, height: u64, round: u32) -> bool {
        let votes = self.votes.read().await;
        let validators = self.validators.read().await;
        
        if let Some(precommits) = votes.get(&(height, round, VoteType::Precommit)) {
            precommits.len() >= (validators.len() * 2 / 3 + 1)
        } else {
            false
        }
    }

    async fn start_new_round(&self) {
        let mut round = self.round.write().await;
        *round += 1;
        
        let mut state = self.state.write().await;
        *state = ConsensusState::Propose;
    }

    async fn reset_round_state(&self) {
        let mut votes = self.votes.write().await;
        votes.clear();
        
        let mut locked_block = self.locked_block.write().await;
        *locked_block = None;
        
        let mut locked_round = self.locked_round.write().await;
        *locked_round = None;
        
        let mut valid_block = self.valid_block.write().await;
        *valid_block = None;
        
        let mut valid_round = self.valid_round.write().await;
        *valid_round = None;
    }

    pub async fn get_metrics(&self) -> ConsensusMetrics {
        ConsensusMetrics {
            height: *self.height.read().await,
            round: *self.round.read().await,
            state: self.state.read().await.clone(),
            validator_count: self.validators.read().await.len(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConsensusMetrics {
    pub height: u64,
    pub round: u32,
    pub state: ConsensusState,
    pub validator_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_consensus_basic() {
        let validators = vec![
            Validator::new(vec![1]),
            Validator::new(vec![2]),
            Validator::new(vec![3]),
            Validator::new(vec![4]),
        ];
        
        let (consensus, mut block_rx) = TendermintConsensus::new(validators);
        
        tokio::spawn(async move {
            consensus.start().await;
        });
        
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}