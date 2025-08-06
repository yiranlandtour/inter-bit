use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

#[derive(Debug, Clone)]
pub struct Validator {
    pub public_key: Vec<u8>,
    pub voting_power: u64,
    pub address: [u8; 20],
    signing_key: Option<SigningKey>,
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
            signing_key: None,
        }
    }

    pub fn with_signing_key(signing_key: SigningKey) -> Self {
        let verifying_key = signing_key.verifying_key();
        let public_key = verifying_key.to_bytes().to_vec();
        let mut address = [0u8; 20];
        address.copy_from_slice(&public_key[..20]);
        
        Self {
            public_key: public_key.clone(),
            voting_power: 1,
            address,
            signing_key: Some(signing_key),
        }
    }

    pub fn sign(&self, message: &[u8]) -> Option<Vec<u8>> {
        self.signing_key.as_ref().map(|key| {
            let signature = key.sign(message);
            signature.to_bytes().to_vec()
        })
    }

    pub fn verify(&self, message: &[u8], signature: &[u8]) -> bool {
        if signature.len() != 64 {
            return false;
        }
        
        let pk_bytes: [u8; 32] = match self.public_key[..32].try_into() {
            Ok(bytes) => bytes,
            Err(_) => return false,
        };
        
        let sig_bytes: [u8; 64] = match signature.try_into() {
            Ok(bytes) => bytes,
            Err(_) => return false,
        };
        
        if let Ok(verifying_key) = VerifyingKey::from_bytes(&pk_bytes) {
            if let Ok(sig) = Signature::from_bytes(&sig_bytes) {
                return verifying_key.verify(message, &sig).is_ok();
            }
        }
        false
    }
}