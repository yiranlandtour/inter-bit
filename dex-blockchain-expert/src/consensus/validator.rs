use ed25519_dalek::{Keypair, PublicKey, Signature, Signer, Verifier};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct Validator {
    pub public_key: Vec<u8>,
    pub voting_power: u64,
    pub address: [u8; 20],
    keypair: Option<Keypair>,
}

impl Validator {
    pub fn new(public_key: Vec<u8>) -> Self {
        let mut address = [0u8; 20];
        if public_key.len() >= 20 {
            address.copy_from_slice(&public_key[..20]);
        }
        
        Self {
            public_key,
            voting_power: 1,
            address,
            keypair: None,
        }
    }

    pub fn with_keypair(keypair: Keypair) -> Self {
        let public_key = keypair.public.to_bytes().to_vec();
        let mut address = [0u8; 20];
        address.copy_from_slice(&public_key[..20]);
        
        Self {
            public_key: public_key.clone(),
            voting_power: 1,
            address,
            keypair: Some(keypair),
        }
    }

    pub fn sign(&self, message: &[u8]) -> Option<Vec<u8>> {
        self.keypair.as_ref().map(|kp| {
            let signature = kp.sign(message);
            signature.to_bytes().to_vec()
        })
    }

    pub fn verify(&self, message: &[u8], signature: &[u8]) -> bool {
        if signature.len() != 64 {
            return false;
        }
        
        let pk_bytes: [u8; 32] = self.public_key[..32].try_into().unwrap_or([0; 32]);
        let sig_bytes: [u8; 64] = signature.try_into().unwrap_or([0; 64]);
        
        if let (Ok(public_key), Ok(sig)) = (
            PublicKey::from_bytes(&pk_bytes),
            Signature::from_bytes(&sig_bytes),
        ) {
            public_key.verify(message, &sig).is_ok()
        } else {
            false
        }
    }
}