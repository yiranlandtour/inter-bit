use super::{PrivacyError, StealthAddress};
use primitive_types::H256;

pub struct StealthAddressManager {
    view_keys: Vec<([u8; 20], Vec<u8>)>,
}

impl StealthAddressManager {
    pub fn new() -> Self {
        Self {
            view_keys: Vec::new(),
        }
    }

    pub async fn generate_stealth_address(&self, recipient: [u8; 20]) -> Result<StealthAddress, PrivacyError> {
        let ephemeral_key = self.generate_ephemeral_key();
        let shared_secret = self.compute_shared_secret(&ephemeral_key, recipient);
        let stealth_pubkey = self.derive_stealth_pubkey(recipient, &shared_secret);
        let view_tag = self.compute_view_tag(&shared_secret);

        Ok(StealthAddress {
            ephemeral_pubkey: ephemeral_key,
            stealth_pubkey,
            view_tag,
        })
    }

    pub async fn check_ownership(&self, address: &StealthAddress, view_key: &[u8]) -> bool {
        let shared_secret = self.recover_shared_secret(&address.ephemeral_pubkey, view_key);
        let expected_tag = self.compute_view_tag(&shared_secret);
        expected_tag == address.view_tag
    }

    fn generate_ephemeral_key(&self) -> Vec<u8> {
        {
            use rand::Rng;
            let mut rng = rand::thread_rng();
            (0..32).map(|_| rng.gen()).collect()
        }
    }

    fn compute_shared_secret(&self, ephemeral_key: &[u8], recipient: [u8; 20]) -> Vec<u8> {
        use sha3::{Digest, Keccak256};
        let mut hasher = Keccak256::new();
        hasher.update(ephemeral_key);
        hasher.update(&recipient);
        hasher.finalize().to_vec()
    }

    fn derive_stealth_pubkey(&self, recipient: [u8; 20], shared_secret: &[u8]) -> Vec<u8> {
        use sha3::{Digest, Keccak256};
        let mut hasher = Keccak256::new();
        hasher.update(&recipient);
        hasher.update(shared_secret);
        hasher.finalize()[..20].to_vec()
    }

    fn compute_view_tag(&self, shared_secret: &[u8]) -> [u8; 4] {
        let mut tag = [0u8; 4];
        tag.copy_from_slice(&shared_secret[..4]);
        tag
    }

    fn recover_shared_secret(&self, ephemeral_pubkey: &[u8], view_key: &[u8]) -> Vec<u8> {
        use sha3::{Digest, Keccak256};
        let mut hasher = Keccak256::new();
        hasher.update(ephemeral_pubkey);
        hasher.update(view_key);
        hasher.finalize().to_vec()
    }
}