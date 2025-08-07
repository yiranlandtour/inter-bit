use primitive_types::{H256, U256};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transaction {
    pub nonce: u64,
    pub from: [u8; 20],
    pub to: Option<[u8; 20]>,
    pub value: u128,
    pub data: Vec<u8>,
    pub gas_limit: u64,
    pub gas_price: u128,
    pub signature: TransactionSignature,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionSignature {
    pub v: u8,
    pub r: [u8; 32],
    pub s: [u8; 32],
}

impl Default for TransactionSignature {
    fn default() -> Self {
        Self {
            v: 0,
            r: [0; 32],
            s: [0; 32],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionReceipt {
    pub transaction_hash: H256,
    pub from: [u8; 20],
    pub to: Option<[u8; 20]>,
    pub gas_used: u64,
    pub status: bool,
    pub logs: Vec<String>,
    pub execution_time_us: u64,
}

impl Transaction {
    pub fn new(
        from: [u8; 20],
        to: Option<[u8; 20]>,
        value: u128,
        data: Vec<u8>,
        nonce: u64,
        gas_limit: u64,
        gas_price: u128,
    ) -> Self {
        Self {
            nonce,
            from,
            to,
            value,
            data,
            gas_limit,
            gas_price,
            signature: TransactionSignature::default(),
        }
    }

    pub fn hash(&self) -> H256 {
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
        H256::from_slice(&result)
    }

    pub fn sign(&mut self, private_key: &[u8; 32]) {
        // 简化的签名实现
        let hash = self.hash();
        let mut r = [0u8; 32];
        let mut s = [0u8; 32];
        
        // 使用私钥和消息哈希生成签名（简化版）
        r[..32].copy_from_slice(&hash.as_bytes()[..32]);
        s[..32].copy_from_slice(private_key);
        
        self.signature = TransactionSignature {
            v: 27,
            r,
            s,
        };
    }

    pub fn verify_signature(&self) -> bool {
        // 简化的签名验证
        // 在实际实现中，应该使用secp256k1库进行ECDSA验证
        !self.signature.r.iter().all(|&b| b == 0) && 
        !self.signature.s.iter().all(|&b| b == 0)
    }

    pub fn sender(&self) -> [u8; 20] {
        self.from
    }

    pub fn gas_cost(&self) -> U256 {
        U256::from(self.gas_limit) * U256::from(self.gas_price)
    }

    pub fn total_cost(&self) -> U256 {
        U256::from(self.value) + self.gas_cost()
    }
}

impl TransactionReceipt {
    pub fn is_success(&self) -> bool {
        self.status
    }

    pub fn gas_cost(&self, gas_price: u128) -> u128 {
        self.gas_used as u128 * gas_price
    }
}