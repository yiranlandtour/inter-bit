use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposalMessage {
    pub height: u64,
    pub round: u32,
    pub block: Vec<u8>,
    pub proposer: Vec<u8>,
    pub signature: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoteMessage {
    pub height: u64,
    pub round: u32,
    pub vote_type: u8,
    pub block_hash: Option<[u8; 32]>,
    pub validator: Vec<u8>,
    pub signature: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeoutMessage {
    pub height: u64,
    pub round: u32,
    pub step: u8,
    pub validator: Vec<u8>,
    pub signature: Vec<u8>,
}