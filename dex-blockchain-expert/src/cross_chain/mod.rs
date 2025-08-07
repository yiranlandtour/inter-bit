pub mod light_client;
pub mod ibc;
pub mod bridge;
pub mod relayer;

use primitive_types::{H256, U256};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossChainConfig {
    pub supported_chains: Vec<ChainInfo>,
    pub relayer_config: RelayerConfig,
    pub bridge_config: BridgeConfig,
    pub verification_threshold: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainInfo {
    pub chain_id: u64,
    pub chain_name: String,
    pub chain_type: ChainType,
    pub consensus_type: ConsensusType,
    pub finality_blocks: u64,
    pub average_block_time: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChainType {
    EVM,
    Cosmos,
    Substrate,
    Solana,
    Bitcoin,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConsensusType {
    ProofOfWork,
    ProofOfStake,
    DelegatedProofOfStake,
    ProofOfAuthority,
    BFT,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayerConfig {
    pub min_confirmations: u64,
    pub batch_size: usize,
    pub relay_interval_ms: u64,
    pub max_retry_attempts: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeConfig {
    pub escrow_address: [u8; 20],
    pub fee_percentage: f64,
    pub min_transfer_amount: U256,
    pub max_transfer_amount: U256,
    pub supported_tokens: Vec<TokenInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenInfo {
    pub symbol: String,
    pub decimals: u8,
    pub addresses: HashMap<u64, H256>,
}

pub struct CrossChainManager {
    config: CrossChainConfig,
    light_clients: Arc<RwLock<HashMap<u64, Arc<light_client::LightClient>>>>,
    bridge: Arc<bridge::Bridge>,
    relayer: Arc<relayer::Relayer>,
    pending_transfers: Arc<RwLock<Vec<PendingTransfer>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingTransfer {
    pub id: H256,
    pub source_chain: u64,
    pub destination_chain: u64,
    pub sender: Vec<u8>,
    pub recipient: Vec<u8>,
    pub token: H256,
    pub amount: U256,
    pub status: TransferStatus,
    pub proof: Option<MerkleProof>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TransferStatus {
    Pending,
    Locked,
    Proving,
    Verified,
    Minting,
    Completed,
    Failed(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MerkleProof {
    pub block_height: u64,
    pub tx_hash: H256,
    pub merkle_path: Vec<H256>,
    pub merkle_root: H256,
}

impl CrossChainManager {
    pub fn new(config: CrossChainConfig) -> Self {
        let bridge = Arc::new(bridge::Bridge::new(config.bridge_config.clone()));
        let relayer = Arc::new(relayer::Relayer::new(config.relayer_config.clone()));
        
        Self {
            config,
            light_clients: Arc::new(RwLock::new(HashMap::new())),
            bridge,
            relayer,
            pending_transfers: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub async fn initialize_chain(&self, chain_info: ChainInfo) -> Result<(), CrossChainError> {
        let light_client = Arc::new(light_client::LightClient::new(chain_info.clone()));
        
        light_client.initialize().await?;
        
        let mut clients = self.light_clients.write().await;
        clients.insert(chain_info.chain_id, light_client);
        
        Ok(())
    }

    pub async fn initiate_transfer(
        &self,
        source_chain: u64,
        destination_chain: u64,
        sender: Vec<u8>,
        recipient: Vec<u8>,
        token: H256,
        amount: U256,
    ) -> Result<H256, CrossChainError> {
        if amount < self.config.bridge_config.min_transfer_amount {
            return Err(CrossChainError::AmountTooLow);
        }
        
        if amount > self.config.bridge_config.max_transfer_amount {
            return Err(CrossChainError::AmountTooHigh);
        }

        let transfer_id = self.bridge.lock_tokens(
            source_chain,
            sender.clone(),
            token,
            amount,
        ).await?;

        let pending_transfer = PendingTransfer {
            id: transfer_id,
            source_chain,
            destination_chain,
            sender,
            recipient,
            token,
            amount,
            status: TransferStatus::Locked,
            proof: None,
        };

        let mut transfers = self.pending_transfers.write().await;
        transfers.push(pending_transfer);

        self.relayer.submit_transfer(transfer_id).await?;

        Ok(transfer_id)
    }

    pub async fn verify_transfer(&self, transfer_id: H256) -> Result<bool, CrossChainError> {
        let transfers = self.pending_transfers.read().await;
        let transfer = transfers
            .iter()
            .find(|t| t.id == transfer_id)
            .ok_or(CrossChainError::TransferNotFound)?;

        let clients = self.light_clients.read().await;
        let source_client = clients
            .get(&transfer.source_chain)
            .ok_or(CrossChainError::ChainNotSupported)?;

        if let Some(proof) = &transfer.proof {
            source_client.verify_transaction(proof).await
        } else {
            Ok(false)
        }
    }

    pub async fn complete_transfer(&self, transfer_id: H256) -> Result<(), CrossChainError> {
        let mut transfers = self.pending_transfers.write().await;
        let transfer = transfers
            .iter_mut()
            .find(|t| t.id == transfer_id)
            .ok_or(CrossChainError::TransferNotFound)?;

        if !matches!(transfer.status, TransferStatus::Verified) {
            return Err(CrossChainError::TransferNotVerified);
        }

        transfer.status = TransferStatus::Minting;

        self.bridge.mint_tokens(
            transfer.destination_chain,
            transfer.recipient.clone(),
            transfer.token,
            transfer.amount,
        ).await?;

        transfer.status = TransferStatus::Completed;

        Ok(())
    }

    pub async fn get_transfer_status(&self, transfer_id: H256) -> Option<TransferStatus> {
        let transfers = self.pending_transfers.read().await;
        transfers
            .iter()
            .find(|t| t.id == transfer_id)
            .map(|t| t.status.clone())
    }

    pub async fn update_light_client(&self, chain_id: u64, block_header: Vec<u8>) -> Result<(), CrossChainError> {
        let mut clients = self.light_clients.write().await;
        let client = clients
            .get_mut(&chain_id)
            .ok_or(CrossChainError::ChainNotSupported)?;

        client.update_header(block_header).await
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum CrossChainError {
    ChainNotSupported,
    InvalidProof,
    TransferNotFound,
    TransferNotVerified,
    AmountTooLow,
    AmountTooHigh,
    RelayerError(String),
    BridgeError(String),
    LightClientError(String),
}

impl std::error::Error for CrossChainError {}

impl std::fmt::Display for CrossChainError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            CrossChainError::ChainNotSupported => write!(f, "Chain not supported"),
            CrossChainError::InvalidProof => write!(f, "Invalid proof"),
            CrossChainError::TransferNotFound => write!(f, "Transfer not found"),
            CrossChainError::TransferNotVerified => write!(f, "Transfer not verified"),
            CrossChainError::AmountTooLow => write!(f, "Amount too low"),
            CrossChainError::AmountTooHigh => write!(f, "Amount too high"),
            CrossChainError::RelayerError(e) => write!(f, "Relayer error: {}", e),
            CrossChainError::BridgeError(e) => write!(f, "Bridge error: {}", e),
            CrossChainError::LightClientError(e) => write!(f, "Light client error: {}", e),
        }
    }
}