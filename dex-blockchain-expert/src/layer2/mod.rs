pub mod optimistic_rollup;
pub mod zk_rollup;
pub mod state_channel;
pub mod data_availability;

use primitive_types::{H256, U256};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Layer2Config {
    pub rollup_type: RollupType,
    pub batch_size: usize,
    pub batch_timeout_ms: u64,
    pub challenge_period_blocks: u64,
    pub min_bond_amount: U256,
    pub data_availability_config: DataAvailabilityConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RollupType {
    Optimistic,
    ZkRollup,
    Hybrid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataAvailabilityConfig {
    pub storage_type: StorageType,
    pub compression_enabled: bool,
    pub erasure_coding_enabled: bool,
    pub replication_factor: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StorageType {
    OnChain,
    IPFS,
    Celestia,
    EigenDA,
    Custom,
}

pub struct Layer2Manager {
    config: Layer2Config,
    optimistic_rollup: Option<Arc<optimistic_rollup::OptimisticRollup>>,
    zk_rollup: Option<Arc<zk_rollup::ZkRollup>>,
    state_channels: Arc<RwLock<Vec<state_channel::StateChannel>>>,
    da_layer: Arc<data_availability::DataAvailabilityLayer>,
}

impl Layer2Manager {
    pub fn new(config: Layer2Config) -> Self {
        let da_layer = Arc::new(data_availability::DataAvailabilityLayer::new(
            config.data_availability_config.clone(),
        ));

        let optimistic_rollup = match config.rollup_type {
            RollupType::Optimistic | RollupType::Hybrid => {
                Some(Arc::new(optimistic_rollup::OptimisticRollup::new(
                    config.clone(),
                    Arc::clone(&da_layer),
                )))
            }
            _ => None,
        };

        let zk_rollup = match config.rollup_type {
            RollupType::ZkRollup | RollupType::Hybrid => {
                Some(Arc::new(zk_rollup::ZkRollup::new(
                    config.clone(),
                    Arc::clone(&da_layer),
                )))
            }
            _ => None,
        };

        Self {
            config,
            optimistic_rollup,
            zk_rollup,
            state_channels: Arc::new(RwLock::new(Vec::new())),
            da_layer,
        }
    }

    pub async fn submit_transaction(&self, tx: Transaction) -> Result<H256, Layer2Error> {
        match self.config.rollup_type {
            RollupType::Optimistic => {
                if let Some(rollup) = &self.optimistic_rollup {
                    rollup.submit_transaction(tx).await
                } else {
                    Err(Layer2Error::RollupNotInitialized)
                }
            }
            RollupType::ZkRollup => {
                if let Some(rollup) = &self.zk_rollup {
                    rollup.submit_transaction(tx).await
                } else {
                    Err(Layer2Error::RollupNotInitialized)
                }
            }
            RollupType::Hybrid => {
                if tx.requires_instant_finality {
                    if let Some(rollup) = &self.zk_rollup {
                        rollup.submit_transaction(tx).await
                    } else {
                        Err(Layer2Error::RollupNotInitialized)
                    }
                } else {
                    if let Some(rollup) = &self.optimistic_rollup {
                        rollup.submit_transaction(tx).await
                    } else {
                        Err(Layer2Error::RollupNotInitialized)
                    }
                }
            }
        }
    }

    pub async fn create_state_channel(
        &self,
        participants: Vec<[u8; 20]>,
        initial_state: ChannelState,
    ) -> Result<H256, Layer2Error> {
        let channel = state_channel::StateChannel::new(participants, initial_state);
        let channel_id = channel.id();
        
        let mut channels = self.state_channels.write().await;
        channels.push(channel);
        
        Ok(channel_id)
    }

    pub async fn get_batch_status(&self, batch_id: H256) -> Option<BatchStatus> {
        if let Some(rollup) = &self.optimistic_rollup {
            return rollup.get_batch_status(batch_id).await;
        }
        
        if let Some(rollup) = &self.zk_rollup {
            return rollup.get_batch_status(batch_id).await;
        }
        
        None
    }

    pub async fn challenge_state_root(
        &self,
        batch_id: H256,
        challenger: [u8; 20],
        proof: FraudProof,
    ) -> Result<(), Layer2Error> {
        if let Some(rollup) = &self.optimistic_rollup {
            rollup.challenge_state_root(batch_id, challenger, proof).await
        } else {
            Err(Layer2Error::RollupNotInitialized)
        }
    }

    pub async fn finalize_batch(&self, batch_id: H256) -> Result<(), Layer2Error> {
        match self.config.rollup_type {
            RollupType::Optimistic => {
                if let Some(rollup) = &self.optimistic_rollup {
                    rollup.finalize_batch(batch_id).await
                } else {
                    Err(Layer2Error::RollupNotInitialized)
                }
            }
            RollupType::ZkRollup => {
                if let Some(rollup) = &self.zk_rollup {
                    rollup.finalize_batch(batch_id).await
                } else {
                    Err(Layer2Error::RollupNotInitialized)
                }
            }
            RollupType::Hybrid => {
                let mut finalized = false;
                
                if let Some(rollup) = &self.optimistic_rollup {
                    if rollup.has_batch(batch_id).await {
                        rollup.finalize_batch(batch_id).await?;
                        finalized = true;
                    }
                }
                
                if !finalized {
                    if let Some(rollup) = &self.zk_rollup {
                        rollup.finalize_batch(batch_id).await?;
                    }
                }
                
                Ok(())
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transaction {
    pub id: H256,
    pub from: [u8; 20],
    pub to: [u8; 20],
    pub value: U256,
    pub data: Vec<u8>,
    pub nonce: u64,
    pub gas_price: U256,
    pub gas_limit: u64,
    pub signature: Vec<u8>,
    pub requires_instant_finality: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchStatus {
    pub batch_id: H256,
    pub state: BatchState,
    pub num_transactions: usize,
    pub state_root: H256,
    pub timestamp: u64,
    pub proposer: [u8; 20],
    pub finalized: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum BatchState {
    Pending,
    Submitted,
    Challenged,
    Finalized,
    Invalid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FraudProof {
    pub batch_id: H256,
    pub invalid_state_root: H256,
    pub correct_state_root: H256,
    pub witness_data: Vec<u8>,
    pub execution_trace: Vec<ExecutionStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionStep {
    pub pc: u64,
    pub opcode: u8,
    pub stack: Vec<U256>,
    pub memory: Vec<u8>,
    pub storage: Vec<(H256, H256)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelState {
    pub balances: Vec<U256>,
    pub nonce: u64,
    pub merkle_root: H256,
}

#[derive(Debug)]
pub enum Layer2Error {
    RollupNotInitialized,
    InvalidTransaction,
    BatchNotFound,
    ChallengePeriodNotExpired,
    InvalidProof,
    InsufficientBond,
    ChannelNotFound,
    DataNotAvailable,
    CompressionError,
    VerificationFailed,
}

impl std::error::Error for Layer2Error {}

impl std::fmt::Display for Layer2Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Layer2Error::RollupNotInitialized => write!(f, "Rollup not initialized"),
            Layer2Error::InvalidTransaction => write!(f, "Invalid transaction"),
            Layer2Error::BatchNotFound => write!(f, "Batch not found"),
            Layer2Error::ChallengePeriodNotExpired => write!(f, "Challenge period not expired"),
            Layer2Error::InvalidProof => write!(f, "Invalid proof"),
            Layer2Error::InsufficientBond => write!(f, "Insufficient bond"),
            Layer2Error::ChannelNotFound => write!(f, "Channel not found"),
            Layer2Error::DataNotAvailable => write!(f, "Data not available"),
            Layer2Error::CompressionError => write!(f, "Compression error"),
            Layer2Error::VerificationFailed => write!(f, "Verification failed"),
        }
    }
}