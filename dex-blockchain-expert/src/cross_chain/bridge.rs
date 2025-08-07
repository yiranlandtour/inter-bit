use super::{BridgeConfig, CrossChainError};
use primitive_types::{H256, U256};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockedAsset {
    pub chain_id: u64,
    pub token: H256,
    pub owner: Vec<u8>,
    pub amount: U256,
    pub lock_time: u64,
    pub unlock_time: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MintedAsset {
    pub chain_id: u64,
    pub token: H256,
    pub recipient: Vec<u8>,
    pub amount: U256,
    pub mint_time: u64,
    pub source_tx: H256,
}

pub struct Bridge {
    config: BridgeConfig,
    locked_assets: Arc<RwLock<HashMap<H256, LockedAsset>>>,
    minted_assets: Arc<RwLock<HashMap<H256, MintedAsset>>>,
    token_mappings: Arc<RwLock<HashMap<(u64, H256), H256>>>,
    liquidity_pools: Arc<RwLock<HashMap<H256, LiquidityPool>>>,
}

#[derive(Debug, Clone)]
struct LiquidityPool {
    token: H256,
    total_liquidity: U256,
    available_liquidity: U256,
    pending_withdrawals: U256,
}

impl Bridge {
    pub fn new(config: BridgeConfig) -> Self {
        Self {
            config,
            locked_assets: Arc::new(RwLock::new(HashMap::new())),
            minted_assets: Arc::new(RwLock::new(HashMap::new())),
            token_mappings: Arc::new(RwLock::new(HashMap::new())),
            liquidity_pools: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn lock_tokens(
        &self,
        chain_id: u64,
        owner: Vec<u8>,
        token: H256,
        amount: U256,
    ) -> Result<H256, CrossChainError> {
        if amount < self.config.min_transfer_amount {
            return Err(CrossChainError::AmountTooLow);
        }

        if amount > self.config.max_transfer_amount {
            return Err(CrossChainError::AmountTooHigh);
        }

        let lock_id = self.generate_lock_id(&owner, token, amount);
        
        let locked_asset = LockedAsset {
            chain_id,
            token,
            owner,
            amount,
            lock_time: self.get_current_timestamp(),
            unlock_time: None,
        };

        let mut locked = self.locked_assets.write().await;
        locked.insert(lock_id, locked_asset);

        self.update_liquidity_pool(token, amount, true).await?;

        Ok(lock_id)
    }

    pub async fn unlock_tokens(&self, lock_id: H256) -> Result<(), CrossChainError> {
        let mut locked = self.locked_assets.write().await;
        let asset = locked
            .get_mut(&lock_id)
            .ok_or(CrossChainError::BridgeError("Lock not found".to_string()))?;

        if asset.unlock_time.is_some() {
            return Err(CrossChainError::BridgeError("Already unlocked".to_string()));
        }

        asset.unlock_time = Some(self.get_current_timestamp());

        self.update_liquidity_pool(asset.token, asset.amount, false).await?;

        Ok(())
    }

    pub async fn mint_tokens(
        &self,
        chain_id: u64,
        recipient: Vec<u8>,
        token: H256,
        amount: U256,
    ) -> Result<H256, CrossChainError> {
        let wrapped_token = self.get_wrapped_token(chain_id, token).await?;
        
        let mint_id = H256::random();
        let minted_asset = MintedAsset {
            chain_id,
            token: wrapped_token,
            recipient,
            amount,
            mint_time: self.get_current_timestamp(),
            source_tx: mint_id,
        };

        let mut minted = self.minted_assets.write().await;
        minted.insert(mint_id, minted_asset);

        Ok(mint_id)
    }

    pub async fn burn_tokens(
        &self,
        chain_id: u64,
        owner: Vec<u8>,
        token: H256,
        amount: U256,
    ) -> Result<H256, CrossChainError> {
        let burn_id = H256::random();
        
        Ok(burn_id)
    }

    async fn get_wrapped_token(&self, chain_id: u64, original_token: H256) -> Result<H256, CrossChainError> {
        let mappings = self.token_mappings.read().await;
        
        mappings
            .get(&(chain_id, original_token))
            .copied()
            .ok_or(CrossChainError::BridgeError("Token mapping not found".to_string()))
            .or_else(|_| {
                Ok(self.derive_wrapped_token_address(chain_id, original_token))
            })
    }

    fn derive_wrapped_token_address(&self, chain_id: u64, original_token: H256) -> H256 {
        use sha3::{Digest, Keccak256};
        
        let mut hasher = Keccak256::new();
        hasher.update(b"WRAPPED_TOKEN");
        hasher.update(&chain_id.to_le_bytes());
        hasher.update(&original_token.0);
        
        H256::from_slice(&hasher.finalize())
    }

    async fn update_liquidity_pool(
        &self,
        token: H256,
        amount: U256,
        is_lock: bool,
    ) -> Result<(), CrossChainError> {
        let mut pools = self.liquidity_pools.write().await;
        
        let pool = pools.entry(token).or_insert_with(|| LiquidityPool {
            token,
            total_liquidity: U256::zero(),
            available_liquidity: U256::zero(),
            pending_withdrawals: U256::zero(),
        });

        if is_lock {
            pool.total_liquidity = pool.total_liquidity + amount;
            pool.available_liquidity = pool.available_liquidity + amount;
        } else {
            if pool.available_liquidity < amount {
                return Err(CrossChainError::BridgeError("Insufficient liquidity".to_string()));
            }
            pool.available_liquidity = pool.available_liquidity - amount;
        }

        Ok(())
    }

    fn generate_lock_id(&self, owner: &[u8], token: H256, amount: U256) -> H256 {
        use sha3::{Digest, Keccak256};
        
        let mut hasher = Keccak256::new();
        hasher.update(owner);
        hasher.update(&token.0);
        hasher.update(&amount.to_little_endian());
        hasher.update(&self.get_current_timestamp().to_le_bytes());
        
        H256::from_slice(&hasher.finalize())
    }

    fn get_current_timestamp(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    pub async fn add_token_mapping(
        &self,
        source_chain: u64,
        source_token: H256,
        dest_chain: u64,
        dest_token: H256,
    ) -> Result<(), CrossChainError> {
        let mut mappings = self.token_mappings.write().await;
        mappings.insert((source_chain, source_token), dest_token);
        mappings.insert((dest_chain, dest_token), source_token);
        Ok(())
    }

    pub async fn get_locked_amount(&self, token: H256) -> U256 {
        let locked = self.locked_assets.read().await;
        locked
            .values()
            .filter(|asset| asset.token == token && asset.unlock_time.is_none())
            .fold(U256::zero(), |acc, asset| acc + asset.amount)
    }

    pub async fn get_minted_amount(&self, token: H256) -> U256 {
        let minted = self.minted_assets.read().await;
        minted
            .values()
            .filter(|asset| asset.token == token)
            .fold(U256::zero(), |acc, asset| acc + asset.amount)
    }

    pub async fn calculate_fee(&self, amount: U256) -> U256 {
        let fee_basis_points = (self.config.fee_percentage * 10000.0) as u64;
        amount * U256::from(fee_basis_points) / U256::from(10000u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_lock_and_unlock() {
        let config = BridgeConfig {
            escrow_address: [0u8; 20],
            fee_percentage: 0.1,
            min_transfer_amount: U256::from(100u64),
            max_transfer_amount: U256::from(1_000_000u64),
            supported_tokens: vec![],
        };

        let bridge = Bridge::new(config);
        
        let lock_id = bridge.lock_tokens(
            1,
            vec![1u8; 20],
            H256::random(),
            U256::from(1000u64),
        ).await.unwrap();

        let result = bridge.unlock_tokens(lock_id).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_mint_tokens() {
        let config = BridgeConfig {
            escrow_address: [0u8; 20],
            fee_percentage: 0.1,
            min_transfer_amount: U256::from(100u64),
            max_transfer_amount: U256::from(1_000_000u64),
            supported_tokens: vec![],
        };

        let bridge = Bridge::new(config);
        
        let mint_id = bridge.mint_tokens(
            2,
            vec![2u8; 20],
            H256::random(),
            U256::from(1000u64),
        ).await.unwrap();

        assert_ne!(mint_id, H256::zero());
    }
}