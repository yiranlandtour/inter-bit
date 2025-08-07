use super::CrossChainError;
use primitive_types::{H256, U256};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IBCChannel {
    pub channel_id: String,
    pub port_id: String,
    pub counterparty_channel_id: String,
    pub counterparty_port_id: String,
    pub connection_id: String,
    pub state: ChannelState,
    pub version: String,
    pub ordering: ChannelOrder,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ChannelState {
    Init,
    TryOpen,
    Open,
    Closed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChannelOrder {
    Ordered,
    Unordered,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IBCPacket {
    pub sequence: u64,
    pub source_port: String,
    pub source_channel: String,
    pub destination_port: String,
    pub destination_channel: String,
    pub data: Vec<u8>,
    pub timeout_height: u64,
    pub timeout_timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IBCConnection {
    pub connection_id: String,
    pub client_id: String,
    pub counterparty_connection_id: String,
    pub counterparty_client_id: String,
    pub state: ConnectionState,
    pub versions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ConnectionState {
    Init,
    TryOpen,
    Open,
}

pub struct IBCHandler {
    channels: Arc<RwLock<HashMap<String, IBCChannel>>>,
    connections: Arc<RwLock<HashMap<String, IBCConnection>>>,
    packet_commitments: Arc<RwLock<HashMap<(String, u64), H256>>>,
    packet_receipts: Arc<RwLock<HashMap<(String, u64), bool>>>,
    packet_acknowledgements: Arc<RwLock<HashMap<(String, u64), Vec<u8>>>>,
    next_sequence: Arc<RwLock<HashMap<String, u64>>>,
}

impl IBCHandler {
    pub fn new() -> Self {
        Self {
            channels: Arc::new(RwLock::new(HashMap::new())),
            connections: Arc::new(RwLock::new(HashMap::new())),
            packet_commitments: Arc::new(RwLock::new(HashMap::new())),
            packet_receipts: Arc::new(RwLock::new(HashMap::new())),
            packet_acknowledgements: Arc::new(RwLock::new(HashMap::new())),
            next_sequence: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn create_channel(
        &self,
        port_id: String,
        connection_id: String,
        version: String,
        ordering: ChannelOrder,
    ) -> Result<String, CrossChainError> {
        let channel_id = format!("channel-{}", self.generate_id());
        
        let channel = IBCChannel {
            channel_id: channel_id.clone(),
            port_id,
            counterparty_channel_id: String::new(),
            counterparty_port_id: String::new(),
            connection_id,
            state: ChannelState::Init,
            version,
            ordering,
        };

        let mut channels = self.channels.write().await;
        channels.insert(channel_id.clone(), channel);

        Ok(channel_id)
    }

    pub async fn channel_open_try(
        &self,
        channel_id: String,
        counterparty_channel_id: String,
        counterparty_port_id: String,
    ) -> Result<(), CrossChainError> {
        let mut channels = self.channels.write().await;
        let channel = channels
            .get_mut(&channel_id)
            .ok_or(CrossChainError::LightClientError("Channel not found".to_string()))?;

        if channel.state != ChannelState::Init {
            return Err(CrossChainError::LightClientError("Invalid channel state".to_string()));
        }

        channel.counterparty_channel_id = counterparty_channel_id;
        channel.counterparty_port_id = counterparty_port_id;
        channel.state = ChannelState::TryOpen;

        Ok(())
    }

    pub async fn channel_open_confirm(&self, channel_id: String) -> Result<(), CrossChainError> {
        let mut channels = self.channels.write().await;
        let channel = channels
            .get_mut(&channel_id)
            .ok_or(CrossChainError::LightClientError("Channel not found".to_string()))?;

        if channel.state != ChannelState::TryOpen {
            return Err(CrossChainError::LightClientError("Invalid channel state".to_string()));
        }

        channel.state = ChannelState::Open;

        Ok(())
    }

    pub async fn send_packet(
        &self,
        channel_id: String,
        port_id: String,
        data: Vec<u8>,
        timeout_height: u64,
    ) -> Result<u64, CrossChainError> {
        let channels = self.channels.read().await;
        let channel = channels
            .get(&channel_id)
            .ok_or(CrossChainError::LightClientError("Channel not found".to_string()))?;

        if channel.state != ChannelState::Open {
            return Err(CrossChainError::LightClientError("Channel not open".to_string()));
        }

        let mut sequences = self.next_sequence.write().await;
        let sequence = sequences.entry(channel_id.clone()).or_insert(1);
        let packet_sequence = *sequence;
        *sequence += 1;

        let packet = IBCPacket {
            sequence: packet_sequence,
            source_port: port_id,
            source_channel: channel_id.clone(),
            destination_port: channel.counterparty_port_id.clone(),
            destination_channel: channel.counterparty_channel_id.clone(),
            data,
            timeout_height,
            timeout_timestamp: self.get_current_timestamp() + 3600,
        };

        let commitment = self.compute_packet_commitment(&packet);
        let mut commitments = self.packet_commitments.write().await;
        commitments.insert((channel_id, packet_sequence), commitment);

        Ok(packet_sequence)
    }

    pub async fn receive_packet(
        &self,
        packet: IBCPacket,
        proof: Vec<u8>,
    ) -> Result<Vec<u8>, CrossChainError> {
        let channels = self.channels.read().await;
        let channel = channels
            .get(&packet.destination_channel)
            .ok_or(CrossChainError::LightClientError("Channel not found".to_string()))?;

        if channel.state != ChannelState::Open {
            return Err(CrossChainError::LightClientError("Channel not open".to_string()));
        }

        if !self.verify_packet_commitment(&packet, &proof).await? {
            return Err(CrossChainError::InvalidProof);
        }

        let mut receipts = self.packet_receipts.write().await;
        receipts.insert((packet.destination_channel.clone(), packet.sequence), true);

        let acknowledgement = self.process_packet_data(&packet.data).await?;
        
        let mut acknowledgements = self.packet_acknowledgements.write().await;
        acknowledgements.insert(
            (packet.destination_channel, packet.sequence),
            acknowledgement.clone(),
        );

        Ok(acknowledgement)
    }

    pub async fn acknowledge_packet(
        &self,
        channel_id: String,
        sequence: u64,
        acknowledgement: Vec<u8>,
        proof: Vec<u8>,
    ) -> Result<(), CrossChainError> {
        let channels = self.channels.read().await;
        let _channel = channels
            .get(&channel_id)
            .ok_or(CrossChainError::LightClientError("Channel not found".to_string()))?;

        if !self.verify_acknowledgement(&channel_id, sequence, &acknowledgement, &proof).await? {
            return Err(CrossChainError::InvalidProof);
        }

        let mut commitments = self.packet_commitments.write().await;
        commitments.remove(&(channel_id, sequence));

        Ok(())
    }

    async fn verify_packet_commitment(&self, _packet: &IBCPacket, _proof: &[u8]) -> Result<bool, CrossChainError> {
        Ok(true)
    }

    async fn verify_acknowledgement(
        &self,
        _channel_id: &str,
        _sequence: u64,
        _acknowledgement: &[u8],
        _proof: &[u8],
    ) -> Result<bool, CrossChainError> {
        Ok(true)
    }

    async fn process_packet_data(&self, data: &[u8]) -> Result<Vec<u8>, CrossChainError> {
        Ok(vec![1])
    }

    fn compute_packet_commitment(&self, packet: &IBCPacket) -> H256 {
        use sha3::{Digest, Keccak256};
        
        let mut hasher = Keccak256::new();
        hasher.update(&packet.sequence.to_le_bytes());
        hasher.update(packet.source_port.as_bytes());
        hasher.update(packet.source_channel.as_bytes());
        hasher.update(packet.destination_port.as_bytes());
        hasher.update(packet.destination_channel.as_bytes());
        hasher.update(&packet.data);
        hasher.update(&packet.timeout_height.to_le_bytes());
        hasher.update(&packet.timeout_timestamp.to_le_bytes());
        
        H256::from_slice(&hasher.finalize())
    }

    fn generate_id(&self) -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64
    }

    fn get_current_timestamp(&self) -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    pub async fn get_channel(&self, channel_id: &str) -> Option<IBCChannel> {
        let channels = self.channels.read().await;
        channels.get(channel_id).cloned()
    }

    pub async fn close_channel(&self, channel_id: String) -> Result<(), CrossChainError> {
        let mut channels = self.channels.write().await;
        let channel = channels
            .get_mut(&channel_id)
            .ok_or(CrossChainError::LightClientError("Channel not found".to_string()))?;

        channel.state = ChannelState::Closed;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_channel_lifecycle() {
        let handler = IBCHandler::new();
        
        let channel_id = handler.create_channel(
            "transfer".to_string(),
            "connection-0".to_string(),
            "ics20-1".to_string(),
            ChannelOrder::Unordered,
        ).await.unwrap();

        handler.channel_open_try(
            channel_id.clone(),
            "channel-1".to_string(),
            "transfer".to_string(),
        ).await.unwrap();

        handler.channel_open_confirm(channel_id.clone()).await.unwrap();

        let channel = handler.get_channel(&channel_id).await.unwrap();
        assert_eq!(channel.state, ChannelState::Open);
    }

    #[tokio::test]
    async fn test_packet_send_receive() {
        let handler = IBCHandler::new();
        
        let channel_id = handler.create_channel(
            "transfer".to_string(),
            "connection-0".to_string(),
            "ics20-1".to_string(),
            ChannelOrder::Unordered,
        ).await.unwrap();

        handler.channel_open_try(
            channel_id.clone(),
            "channel-1".to_string(),
            "transfer".to_string(),
        ).await.unwrap();

        handler.channel_open_confirm(channel_id.clone()).await.unwrap();

        let sequence = handler.send_packet(
            channel_id.clone(),
            "transfer".to_string(),
            vec![1, 2, 3],
            1000,
        ).await.unwrap();

        assert_eq!(sequence, 1);
    }
}