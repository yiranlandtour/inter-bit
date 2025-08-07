use super::{ChainInfo, ChainType, ConsensusType, CrossChainError, MerkleProof};
use primitive_types::{H256, U256};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockHeader {
    pub chain_id: u64,
    pub height: u64,
    pub timestamp: u64,
    pub previous_hash: H256,
    pub state_root: H256,
    pub transactions_root: H256,
    pub receipts_root: H256,
    pub validator_set_hash: H256,
    pub signatures: Vec<Signature>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signature {
    pub validator: [u8; 20],
    pub signature: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct ValidatorSet {
    pub validators: Vec<Validator>,
    pub total_voting_power: u64,
    pub threshold: u64,
}

#[derive(Debug, Clone)]
pub struct Validator {
    pub address: [u8; 20],
    pub public_key: Vec<u8>,
    pub voting_power: u64,
}

pub struct LightClient {
    chain_info: ChainInfo,
    trusted_headers: Arc<RwLock<VecDeque<BlockHeader>>>,
    validator_sets: Arc<RwLock<HashMap<u64, ValidatorSet>>>,
    sync_state: Arc<RwLock<SyncState>>,
    max_clock_drift: u64,
    trusting_period: u64,
}

#[derive(Debug, Clone)]
struct SyncState {
    latest_height: u64,
    latest_timestamp: u64,
    syncing: bool,
    last_error: Option<String>,
}

impl LightClient {
    pub fn new(chain_info: ChainInfo) -> Self {
        Self {
            chain_info,
            trusted_headers: Arc::new(RwLock::new(VecDeque::new())),
            validator_sets: Arc::new(RwLock::new(HashMap::new())),
            sync_state: Arc::new(RwLock::new(SyncState {
                latest_height: 0,
                latest_timestamp: 0,
                syncing: false,
                last_error: None,
            })),
            max_clock_drift: 10,
            trusting_period: 86400 * 7,
        }
    }

    pub async fn initialize(&self) -> Result<(), CrossChainError> {
        let mut sync_state = self.sync_state.write().await;
        sync_state.syncing = true;

        match self.chain_info.chain_type {
            ChainType::EVM => self.initialize_evm_client().await?,
            ChainType::Cosmos => self.initialize_cosmos_client().await?,
            ChainType::Substrate => self.initialize_substrate_client().await?,
            _ => return Err(CrossChainError::ChainNotSupported),
        }

        sync_state.syncing = false;
        Ok(())
    }

    pub async fn update_header(&self, header_data: Vec<u8>) -> Result<(), CrossChainError> {
        let header = self.parse_header(header_data)?;
        
        self.verify_header(&header).await?;

        let mut headers = self.trusted_headers.write().await;
        headers.push_back(header.clone());
        
        if headers.len() > 100 {
            headers.pop_front();
        }

        let mut sync_state = self.sync_state.write().await;
        sync_state.latest_height = header.height;
        sync_state.latest_timestamp = header.timestamp;

        Ok(())
    }

    pub async fn verify_transaction(&self, proof: &MerkleProof) -> Result<bool, CrossChainError> {
        let headers = self.trusted_headers.read().await;
        
        let header = headers
            .iter()
            .find(|h| h.height == proof.block_height)
            .ok_or(CrossChainError::InvalidProof)?;

        let computed_root = self.compute_merkle_root(&proof.tx_hash, &proof.merkle_path);
        
        if computed_root != proof.merkle_root {
            return Ok(false);
        }

        if header.transactions_root != proof.merkle_root {
            return Ok(false);
        }

        Ok(true)
    }

    async fn verify_header(&self, header: &BlockHeader) -> Result<(), CrossChainError> {
        let headers = self.trusted_headers.read().await;
        
        if let Some(last_header) = headers.back() {
            if header.height != last_header.height + 1 {
                return Err(CrossChainError::InvalidProof);
            }
            
            if header.previous_hash != self.hash_header(last_header) {
                return Err(CrossChainError::InvalidProof);
            }
            
            let time_diff = header.timestamp.saturating_sub(last_header.timestamp);
            if time_diff > self.chain_info.average_block_time * 2 {
                return Err(CrossChainError::InvalidProof);
            }
        }

        match self.chain_info.consensus_type {
            ConsensusType::ProofOfStake | ConsensusType::BFT => {
                self.verify_signatures(header).await?;
            }
            ConsensusType::ProofOfWork => {
                self.verify_proof_of_work(header)?;
            }
            _ => {}
        }

        Ok(())
    }

    async fn verify_signatures(&self, header: &BlockHeader) -> Result<(), CrossChainError> {
        let validator_sets = self.validator_sets.read().await;
        
        let validator_set = validator_sets
            .get(&header.height)
            .or_else(|| validator_sets.values().last())
            .ok_or(CrossChainError::InvalidProof)?;

        let mut total_voting_power = 0u64;
        let header_hash = self.hash_header(header);

        for sig in &header.signatures {
            if let Some(validator) = validator_set.validators.iter().find(|v| v.address == sig.validator) {
                if self.verify_signature(&header_hash, &sig.signature, &validator.public_key) {
                    total_voting_power += validator.voting_power;
                }
            }
        }

        if total_voting_power < validator_set.threshold {
            return Err(CrossChainError::InvalidProof);
        }

        Ok(())
    }

    fn verify_proof_of_work(&self, header: &BlockHeader) -> Result<(), CrossChainError> {
        Ok(())
    }

    fn verify_signature(&self, message: &H256, signature: &[u8], public_key: &[u8]) -> bool {
        use ed25519_dalek::{Signature, VerifyingKey, Verifier};
        
        if signature.len() != 64 || public_key.len() != 32 {
            return false;
        }

        let Ok(sig) = Signature::from_bytes(signature.try_into().unwrap()) else {
            return false;
        };

        let Ok(key) = VerifyingKey::from_bytes(public_key.try_into().unwrap()) else {
            return false;
        };

        key.verify(&message.0, &sig).is_ok()
    }

    fn hash_header(&self, header: &BlockHeader) -> H256 {
        use sha3::{Digest, Keccak256};
        
        let mut hasher = Keccak256::new();
        hasher.update(&header.chain_id.to_le_bytes());
        hasher.update(&header.height.to_le_bytes());
        hasher.update(&header.timestamp.to_le_bytes());
        hasher.update(&header.previous_hash.0);
        hasher.update(&header.state_root.0);
        hasher.update(&header.transactions_root.0);
        hasher.update(&header.receipts_root.0);
        hasher.update(&header.validator_set_hash.0);
        
        H256::from_slice(&hasher.finalize())
    }

    fn compute_merkle_root(&self, leaf: &H256, path: &[H256]) -> H256 {
        use sha3::{Digest, Keccak256};
        
        let mut current = *leaf;
        
        for sibling in path {
            let mut hasher = Keccak256::new();
            
            if current < *sibling {
                hasher.update(&current.0);
                hasher.update(&sibling.0);
            } else {
                hasher.update(&sibling.0);
                hasher.update(&current.0);
            }
            
            current = H256::from_slice(&hasher.finalize());
        }
        
        current
    }

    fn parse_header(&self, data: Vec<u8>) -> Result<BlockHeader, CrossChainError> {
        bincode::deserialize(&data)
            .map_err(|e| CrossChainError::LightClientError(e.to_string()))
    }

    async fn initialize_evm_client(&self) -> Result<(), CrossChainError> {
        let genesis_validators = vec![
            Validator {
                address: [1u8; 20],
                public_key: vec![0u8; 32],
                voting_power: 1000,
            },
        ];

        let genesis_set = ValidatorSet {
            validators: genesis_validators,
            total_voting_power: 1000,
            threshold: 667,
        };

        let mut validator_sets = self.validator_sets.write().await;
        validator_sets.insert(0, genesis_set);

        Ok(())
    }

    async fn initialize_cosmos_client(&self) -> Result<(), CrossChainError> {
        Ok(())
    }

    async fn initialize_substrate_client(&self) -> Result<(), CrossChainError> {
        Ok(())
    }

    pub async fn get_latest_height(&self) -> u64 {
        let sync_state = self.sync_state.read().await;
        sync_state.latest_height
    }

    pub async fn get_header(&self, height: u64) -> Option<BlockHeader> {
        let headers = self.trusted_headers.read().await;
        headers.iter().find(|h| h.height == height).cloned()
    }

    pub async fn is_synced(&self) -> bool {
        let sync_state = self.sync_state.read().await;
        let current_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        
        current_time - sync_state.latest_timestamp < self.chain_info.average_block_time * 3
    }

    pub async fn bisection_verify(
        &self,
        untrusted_header: &BlockHeader,
        trusted_header: &BlockHeader,
    ) -> Result<bool, CrossChainError> {
        if untrusted_header.height <= trusted_header.height {
            return Err(CrossChainError::InvalidProof);
        }

        if untrusted_header.timestamp <= trusted_header.timestamp {
            return Err(CrossChainError::InvalidProof);
        }

        let trust_period_end = trusted_header.timestamp + self.trusting_period;
        if untrusted_header.timestamp > trust_period_end {
            return Err(CrossChainError::LightClientError(
                "Header outside trusting period".to_string()
            ));
        }

        self.verify_header(untrusted_header).await?;
        
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_light_client_initialization() {
        let chain_info = ChainInfo {
            chain_id: 1,
            chain_name: "Ethereum".to_string(),
            chain_type: ChainType::EVM,
            consensus_type: ConsensusType::ProofOfStake,
            finality_blocks: 32,
            average_block_time: 12,
        };

        let client = LightClient::new(chain_info);
        let result = client.initialize().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_merkle_proof_verification() {
        let chain_info = ChainInfo {
            chain_id: 1,
            chain_name: "Test".to_string(),
            chain_type: ChainType::EVM,
            consensus_type: ConsensusType::ProofOfStake,
            finality_blocks: 1,
            average_block_time: 1,
        };

        let client = LightClient::new(chain_info);
        
        let tx_hash = H256::random();
        let merkle_path = vec![H256::random(), H256::random()];
        let merkle_root = client.compute_merkle_root(&tx_hash, &merkle_path);
        
        let proof = MerkleProof {
            block_height: 1,
            tx_hash,
            merkle_path,
            merkle_root,
        };

        let header = BlockHeader {
            chain_id: 1,
            height: 1,
            timestamp: 1000,
            previous_hash: H256::zero(),
            state_root: H256::random(),
            transactions_root: merkle_root,
            receipts_root: H256::random(),
            validator_set_hash: H256::random(),
            signatures: vec![],
        };

        let mut headers = client.trusted_headers.write().await;
        headers.push_back(header);
        drop(headers);

        let result = client.verify_transaction(&proof).await;
        assert!(result.is_ok());
        assert!(result.unwrap());
    }
}