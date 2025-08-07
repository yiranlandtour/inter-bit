use super::{MevProtectionError, ProtectedTransaction};
use primitive_types::H256;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedTransaction {
    pub tx_hash: H256,
    pub encrypted_data: Vec<u8>,
    pub key_shares_required: usize,
    pub participants: Vec<[u8; 20]>,
    pub timestamp: u64,
}

#[derive(Debug, Clone)]
pub struct KeyShare {
    pub share_id: usize,
    pub share_data: Vec<u8>,
    pub validator: [u8; 20],
    pub tx_hash: H256,
}

#[derive(Debug)]
pub struct DecryptionCommitment {
    pub tx_hash: H256,
    pub validator: [u8; 20],
    pub commitment_hash: H256,
    pub reveal_deadline: u64,
}

pub struct ThresholdEncryptionSystem {
    threshold: usize,
    encrypted_pool: Arc<RwLock<HashMap<H256, EncryptedTransaction>>>,
    key_shares: Arc<RwLock<HashMap<H256, Vec<KeyShare>>>>,
    decryption_commitments: Arc<RwLock<HashMap<H256, Vec<DecryptionCommitment>>>>,
    decrypted_txs: Arc<RwLock<Vec<ProtectedTransaction>>>,
}

impl ThresholdEncryptionSystem {
    pub fn new(threshold: usize) -> Self {
        Self {
            threshold,
            encrypted_pool: Arc::new(RwLock::new(HashMap::new())),
            key_shares: Arc::new(RwLock::new(HashMap::new())),
            decryption_commitments: Arc::new(RwLock::new(HashMap::new())),
            decrypted_txs: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub async fn submit_encrypted(
        &self,
        tx: ProtectedTransaction,
    ) -> Result<H256, MevProtectionError> {
        let encrypted_data = self.encrypt_transaction(&tx).await?;
        
        let encrypted_tx = EncryptedTransaction {
            tx_hash: tx.tx_hash,
            encrypted_data,
            key_shares_required: self.threshold,
            participants: self.select_participants().await,
            timestamp: tx.timestamp,
        };

        let mut pool = self.encrypted_pool.write().await;
        pool.insert(tx.tx_hash, encrypted_tx.clone());

        self.distribute_key_shares(&encrypted_tx).await?;
        
        Ok(tx.tx_hash)
    }

    pub async fn submit_key_share(&self, share: KeyShare) -> Result<(), MevProtectionError> {
        if !self.verify_key_share(&share).await {
            return Err(MevProtectionError::DecryptionFailed(
                "Invalid key share".to_string(),
            ));
        }

        let mut shares = self.key_shares.write().await;
        shares
            .entry(share.tx_hash)
            .or_insert_with(Vec::new)
            .push(share.clone());

        let share_count = shares.get(&share.tx_hash).map(|s| s.len()).unwrap_or(0);
        
        if share_count >= self.threshold {
            self.attempt_decryption(share.tx_hash).await?;
        }

        Ok(())
    }

    pub async fn decrypt_batch(
        &self,
        key_shares: Vec<Vec<u8>>,
    ) -> Result<Vec<ProtectedTransaction>, MevProtectionError> {
        if key_shares.len() < self.threshold {
            return Err(MevProtectionError::ThresholdNotMet);
        }

        let pool = self.encrypted_pool.read().await;
        let mut decrypted = Vec::new();

        for (tx_hash, encrypted_tx) in pool.iter() {
            match self.decrypt_with_shares(encrypted_tx, &key_shares).await {
                Ok(tx) => decrypted.push(tx),
                Err(_) => continue,
            }
        }

        let mut decrypted_pool = self.decrypted_txs.write().await;
        decrypted_pool.extend(decrypted.clone());

        Ok(decrypted)
    }

    pub async fn commit_to_decrypt(
        &self,
        tx_hash: H256,
        validator: [u8; 20],
        commitment: H256,
    ) -> Result<(), MevProtectionError> {
        let commitment = DecryptionCommitment {
            tx_hash,
            validator,
            commitment_hash: commitment,
            reveal_deadline: self.get_current_timestamp() + 30,
        };

        let mut commitments = self.decryption_commitments.write().await;
        commitments
            .entry(tx_hash)
            .or_insert_with(Vec::new)
            .push(commitment);

        Ok(())
    }

    pub async fn reveal_decryption(
        &self,
        tx_hash: H256,
        validator: [u8; 20],
        share_data: Vec<u8>,
    ) -> Result<(), MevProtectionError> {
        let commitments = self.decryption_commitments.read().await;
        
        let commitment = commitments
            .get(&tx_hash)
            .and_then(|cs| cs.iter().find(|c| c.validator == validator))
            .ok_or_else(|| {
                MevProtectionError::InvalidCommitment
            })?;

        if self.get_current_timestamp() > commitment.reveal_deadline {
            return Err(MevProtectionError::RevealTimeout);
        }

        if !self.verify_commitment(commitment, &share_data).await {
            return Err(MevProtectionError::InvalidCommitment);
        }

        let share = KeyShare {
            share_id: 0,
            share_data,
            validator,
            tx_hash,
        };

        self.submit_key_share(share).await
    }

    async fn encrypt_transaction(
        &self,
        tx: &ProtectedTransaction,
    ) -> Result<Vec<u8>, MevProtectionError> {
        let mut data = Vec::new();
        data.extend_from_slice(&tx.sender);
        data.extend_from_slice(&tx.nonce.to_le_bytes());
        let mut gas_price_bytes = [0u8; 32];
        tx.gas_price.to_little_endian(&mut gas_price_bytes);
        data.extend_from_slice(&gas_price_bytes);
        data.extend_from_slice(&tx.data);
        
        self.threshold_encrypt(data).await
    }

    async fn threshold_encrypt(&self, data: Vec<u8>) -> Result<Vec<u8>, MevProtectionError> {
        use aes_gcm::{
            aead::{Aead, KeyInit, OsRng},
            Aes256Gcm, Nonce,
        };
        use rand::RngCore;

        let mut key = [0u8; 32];
        OsRng.fill_bytes(&mut key);
        
        let cipher = Aes256Gcm::new_from_slice(&key)
            .map_err(|e| MevProtectionError::EncryptionFailed(e.to_string()))?;
        
        let mut nonce_bytes = [0u8; 12];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);
        
        let ciphertext = cipher
            .encrypt(nonce, data.as_ref())
            .map_err(|e| MevProtectionError::EncryptionFailed(e.to_string()))?;
        
        let mut encrypted = Vec::new();
        encrypted.extend_from_slice(&nonce_bytes);
        encrypted.extend_from_slice(&ciphertext);
        
        Ok(encrypted)
    }

    async fn decrypt_with_shares(
        &self,
        encrypted_tx: &EncryptedTransaction,
        key_shares: &[Vec<u8>],
    ) -> Result<ProtectedTransaction, MevProtectionError> {
        let reconstructed_key = self.reconstruct_key(key_shares).await?;
        
        let decrypted_data = self
            .threshold_decrypt(&encrypted_tx.encrypted_data, &reconstructed_key)
            .await?;
        
        self.parse_transaction(decrypted_data, encrypted_tx.tx_hash)
    }

    async fn reconstruct_key(&self, shares: &[Vec<u8>]) -> Result<Vec<u8>, MevProtectionError> {
        if shares.len() < self.threshold {
            return Err(MevProtectionError::ThresholdNotMet);
        }

        let mut key = vec![0u8; 32];
        for (i, share) in shares.iter().enumerate().take(self.threshold) {
            for (j, byte) in share.iter().enumerate().take(32) {
                key[j] ^= byte;
            }
        }
        
        Ok(key)
    }

    async fn threshold_decrypt(
        &self,
        encrypted_data: &[u8],
        key: &[u8],
    ) -> Result<Vec<u8>, MevProtectionError> {
        use aes_gcm::{
            aead::{Aead, KeyInit},
            Aes256Gcm, Nonce,
        };

        if encrypted_data.len() < 12 {
            return Err(MevProtectionError::DecryptionFailed(
                "Invalid encrypted data".to_string(),
            ));
        }

        let nonce = Nonce::from_slice(&encrypted_data[..12]);
        let ciphertext = &encrypted_data[12..];
        
        let cipher = Aes256Gcm::new_from_slice(key)
            .map_err(|e| MevProtectionError::DecryptionFailed(e.to_string()))?;
        
        cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| MevProtectionError::DecryptionFailed(e.to_string()))
    }

    fn parse_transaction(
        &self,
        data: Vec<u8>,
        tx_hash: H256,
    ) -> Result<ProtectedTransaction, MevProtectionError> {
        if data.len() < 20 + 8 + 32 {
            return Err(MevProtectionError::DecryptionFailed(
                "Invalid transaction data".to_string(),
            ));
        }

        let mut sender = [0u8; 20];
        sender.copy_from_slice(&data[..20]);
        
        let nonce = u64::from_le_bytes(
            data[20..28]
                .try_into()
                .map_err(|_| MevProtectionError::DecryptionFailed("Invalid nonce".to_string()))?,
        );
        
        let mut gas_price_bytes = [0u8; 32];
        gas_price_bytes.copy_from_slice(&data[28..60]);
        let gas_price = primitive_types::U256::from_little_endian(&gas_price_bytes);
        
        let tx_data = data[60..].to_vec();
        
        Ok(ProtectedTransaction {
            tx_hash,
            sender,
            nonce,
            gas_price,
            data: tx_data,
            protection_type: super::ProtectionType::ThresholdEncrypted,
            timestamp: self.get_current_timestamp(),
        })
    }

    async fn select_participants(&self) -> Vec<[u8; 20]> {
        vec![[1u8; 20], [2u8; 20], [3u8; 20], [4u8; 20], [5u8; 20]]
    }

    async fn distribute_key_shares(
        &self,
        _encrypted_tx: &EncryptedTransaction,
    ) -> Result<(), MevProtectionError> {
        Ok(())
    }

    async fn verify_key_share(&self, _share: &KeyShare) -> bool {
        true
    }

    async fn attempt_decryption(&self, tx_hash: H256) -> Result<(), MevProtectionError> {
        let shares = self.key_shares.read().await;
        let encrypted_pool = self.encrypted_pool.read().await;
        
        if let (Some(key_shares), Some(encrypted_tx)) = 
            (shares.get(&tx_hash), encrypted_pool.get(&tx_hash)) {
            
            let share_data: Vec<Vec<u8>> = key_shares.iter().map(|s| s.share_data.clone()).collect();
            
            if share_data.len() >= self.threshold {
                let tx = self.decrypt_with_shares(encrypted_tx, &share_data).await?;
                
                let mut decrypted = self.decrypted_txs.write().await;
                decrypted.push(tx);
            }
        }
        
        Ok(())
    }

    async fn verify_commitment(&self, commitment: &DecryptionCommitment, share_data: &[u8]) -> bool {
        use sha3::{Digest, Keccak256};
        
        let mut hasher = Keccak256::new();
        hasher.update(&commitment.tx_hash.0);
        hasher.update(&commitment.validator);
        hasher.update(share_data);
        
        let computed_hash = H256::from_slice(&hasher.finalize());
        computed_hash == commitment.commitment_hash
    }

    fn get_current_timestamp(&self) -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_threshold_encryption() {
        let system = ThresholdEncryptionSystem::new(3);
        
        let tx = ProtectedTransaction {
            tx_hash: H256::random(),
            sender: [1u8; 20],
            nonce: 1,
            gas_price: primitive_types::U256::from(20_000_000_000u64),
            data: vec![1, 2, 3, 4, 5],
            protection_type: super::super::ProtectionType::ThresholdEncrypted,
            timestamp: 1000,
        };

        let result = system.submit_encrypted(tx).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_key_reconstruction() {
        let system = ThresholdEncryptionSystem::new(3);
        
        let shares = vec![
            vec![1u8; 32],
            vec![2u8; 32],
            vec![3u8; 32],
        ];
        
        let result = system.reconstruct_key(&shares).await;
        assert!(result.is_ok());
        
        let key = result.unwrap();
        assert_eq!(key.len(), 32);
    }
}