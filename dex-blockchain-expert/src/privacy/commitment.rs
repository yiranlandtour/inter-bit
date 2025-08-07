use super::{Commitment, CommitmentScheme, MerkleProof, PrivacyError};
use primitive_types::{H256, U256};
use std::collections::HashMap;

pub struct MerkleTree {
    leaves: Vec<H256>,
    nodes: HashMap<(usize, usize), H256>,
    root_cache: Option<H256>,
}

impl MerkleTree {
    pub fn new() -> Self {
        Self {
            leaves: Vec::new(),
            nodes: HashMap::new(),
            root_cache: None,
        }
    }

    pub fn insert(&mut self, commitment: Commitment) {
        let leaf = commitment.hash();
        self.leaves.push(leaf);
        self.root_cache = None; // Invalidate cache
    }

    pub fn root(&self) -> H256 {
        if self.leaves.is_empty() {
            return H256::zero();
        }
        self.compute_root()
    }

    pub fn generate_proof(&self, leaf: H256) -> MerkleProof {
        let index = self.leaves.iter().position(|&l| l == leaf).unwrap_or(0);
        let (path, indices) = self.compute_proof_path(index);
        
        MerkleProof {
            leaf,
            path,
            indices,
        }
    }

    pub fn verify_proof(&self, proof: &MerkleProof, expected_root: H256) -> bool {
        let computed_root = self.compute_root_from_proof(proof);
        computed_root == expected_root
    }

    fn compute_root(&self) -> H256 {
        if self.leaves.len() == 1 {
            return self.leaves[0];
        }

        let mut current_level = self.leaves.clone();
        
        while current_level.len() > 1 {
            let mut next_level = Vec::new();
            
            for i in (0..current_level.len()).step_by(2) {
                let left = current_level[i];
                let right = if i + 1 < current_level.len() {
                    current_level[i + 1]
                } else {
                    current_level[i]
                };
                
                next_level.push(self.hash_pair(left, right));
            }
            
            current_level = next_level;
        }
        
        current_level[0]
    }

    fn compute_proof_path(&self, index: usize) -> (Vec<H256>, Vec<bool>) {
        let mut path = Vec::new();
        let mut indices = Vec::new();
        let mut current_index = index;
        let mut current_level = self.leaves.clone();
        
        while current_level.len() > 1 {
            let is_right = current_index % 2 == 1;
            indices.push(is_right);
            
            let sibling_index = if is_right {
                current_index - 1
            } else {
                current_index + 1
            };
            
            if sibling_index < current_level.len() {
                path.push(current_level[sibling_index]);
            } else {
                path.push(current_level[current_index]);
            }
            
            current_index /= 2;
            
            // Compute next level
            let mut next_level = Vec::new();
            for i in (0..current_level.len()).step_by(2) {
                let left = current_level[i];
                let right = if i + 1 < current_level.len() {
                    current_level[i + 1]
                } else {
                    current_level[i]
                };
                next_level.push(self.hash_pair(left, right));
            }
            current_level = next_level;
        }
        
        (path, indices)
    }

    fn compute_root_from_proof(&self, proof: &MerkleProof) -> H256 {
        let mut current = proof.leaf;
        
        for (sibling, is_right) in proof.path.iter().zip(proof.indices.iter()) {
            current = if *is_right {
                self.hash_pair(*sibling, current)
            } else {
                self.hash_pair(current, *sibling)
            };
        }
        
        current
    }

    fn hash_pair(&self, left: H256, right: H256) -> H256 {
        use sha3::{Digest, Keccak256};
        let mut hasher = Keccak256::new();
        hasher.update(&left.0);
        hasher.update(&right.0);
        H256::from_slice(&hasher.finalize())
    }
}

pub async fn create_pedersen_commitment(amount: U256) -> Result<(Commitment, Vec<u8>), PrivacyError> {
    let blinding_factor = {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        (0..32).map(|_| rng.gen()).collect()
    };
    
    // Simplified Pedersen commitment: C = g^amount * h^blinding
    let commitment_value = compute_pedersen(amount, &blinding_factor);
    
    Ok((
        Commitment {
            value: commitment_value,
            commitment_type: CommitmentScheme::Pedersen,
        },
        blinding_factor,
    ))
}

pub async fn create_hash_commitment(amount: U256) -> Result<(Commitment, Vec<u8>), PrivacyError> {
    use sha3::{Digest, Keccak256};
    
    let blinding_factor = {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        (0..32).map(|_| rng.gen()).collect()
    };
    let mut hasher = Keccak256::new();
    let mut amount_bytes = [0u8; 32];
    amount.to_little_endian(&mut amount_bytes);
    hasher.update(&amount_bytes);
    hasher.update(&blinding_factor);
    
    Ok((
        Commitment {
            value: hasher.finalize().to_vec(),
            commitment_type: CommitmentScheme::HashBased,
        },
        blinding_factor,
    ))
}

pub async fn create_polynomial_commitment(amount: U256) -> Result<(Commitment, Vec<u8>), PrivacyError> {
    let blinding_factor = {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        (0..32).map(|_| rng.gen()).collect()
    };
    
    // Simplified polynomial commitment
    let commitment_value = compute_polynomial(amount, &blinding_factor);
    
    Ok((
        Commitment {
            value: commitment_value,
            commitment_type: CommitmentScheme::Polynomial,
        },
        blinding_factor,
    ))
}

fn compute_pedersen(amount: U256, blinding: &[u8]) -> Vec<u8> {
    use sha3::{Digest, Keccak256};
    let mut hasher = Keccak256::new();
    hasher.update(b"PEDERSEN");
    let mut amount_bytes = [0u8; 32];
    amount.to_little_endian(&mut amount_bytes);
    hasher.update(&amount_bytes);
    hasher.update(blinding);
    hasher.finalize().to_vec()
}

fn compute_polynomial(amount: U256, blinding: &[u8]) -> Vec<u8> {
    use sha3::{Digest, Keccak256};
    let mut hasher = Keccak256::new();
    hasher.update(b"POLYNOMIAL");
    let mut amount_bytes = [0u8; 32];
    amount.to_little_endian(&mut amount_bytes);
    hasher.update(&amount_bytes);
    hasher.update(blinding);
    hasher.finalize().to_vec()
}