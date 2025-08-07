use super::ChannelState;
use primitive_types::{H256, U256};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateChannel {
    id: H256,
    participants: Vec<[u8; 20]>,
    state: ChannelState,
    nonce: u64,
    signatures: Vec<Vec<u8>>,
}

impl StateChannel {
    pub fn new(participants: Vec<[u8; 20]>, initial_state: ChannelState) -> Self {
        Self {
            id: H256::random(),
            participants,
            state: initial_state,
            nonce: 0,
            signatures: Vec::new(),
        }
    }

    pub fn id(&self) -> H256 {
        self.id
    }

    pub fn update_state(&mut self, new_state: ChannelState, signatures: Vec<Vec<u8>>) -> bool {
        if signatures.len() != self.participants.len() {
            return false;
        }

        self.state = new_state;
        self.nonce += 1;
        self.signatures = signatures;
        true
    }

    pub fn finalize(&self) -> ChannelState {
        self.state.clone()
    }
}