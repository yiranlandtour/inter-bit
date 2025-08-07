use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use std::collections::HashMap;
use bytes::Bytes;
use futures::stream::StreamExt;
use serde::{Deserialize, Serialize};
use primitive_types::H256;
use crate::consensus::tendermint::Transaction;

pub struct P2PNetwork {
    node_id: Vec<u8>,
    peers: Arc<RwLock<HashMap<Vec<u8>, PeerConnection>>>,
    message_handler: Arc<RwLock<MessageHandler>>,
    tx_pool: Arc<RwLock<TransactionPool>>,
}

struct PeerConnection {
    peer_id: Vec<u8>,
    address: String,
    latency_ms: u64,
    bandwidth_mbps: u64,
    message_queue: mpsc::Sender<NetworkMessage>,
}

struct MessageHandler {
    consensus_tx: mpsc::Sender<ConsensusMessage>,
    tx_broadcast_tx: mpsc::Sender<Transaction>,
    block_sync_tx: mpsc::Sender<Block>,
}

pub struct TransactionPool {
    pending: HashMap<primitive_types::H256, Transaction>,
    queued: HashMap<[u8; 20], Vec<Transaction>>,
    max_size: usize,
    max_per_account: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkMessage {
    msg_type: MessageType,
    payload: Vec<u8>,
    sender: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum MessageType {
    Consensus,
    Transaction,
    Block,
    StateSync,
    Ping,
    Pong,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsensusMessage {
    height: u64,
    round: u32,
    msg_type: u8,
    data: Vec<u8>,
}


#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    height: u64,
    hash: [u8; 32],
    parent_hash: [u8; 32],
    transactions: Vec<Transaction>,
    state_root: [u8; 32],
}

impl P2PNetwork {
    pub fn new(node_id: Vec<u8>) -> Self {
        let (consensus_tx, _) = mpsc::channel(1000);
        let (tx_broadcast_tx, _) = mpsc::channel(10000);
        let (block_sync_tx, _) = mpsc::channel(100);
        
        Self {
            node_id,
            peers: Arc::new(RwLock::new(HashMap::new())),
            message_handler: Arc::new(RwLock::new(MessageHandler {
                consensus_tx,
                tx_broadcast_tx,
                block_sync_tx,
            })),
            tx_pool: Arc::new(RwLock::new(TransactionPool::new())),
        }
    }

    pub async fn broadcast_transaction(&self, tx: Transaction) {
        // 添加到交易池
        self.tx_pool.write().await.add_transaction(tx.clone());
        
        // 广播给所有对等节点
        let peers = self.peers.read().await;
        for peer in peers.values() {
            let msg = NetworkMessage {
                msg_type: MessageType::Transaction,
                payload: bincode::serialize(&tx).unwrap(),
                sender: self.node_id.clone(),
            };
            let _ = peer.message_queue.send(msg).await;
        }
    }

    pub async fn broadcast_consensus_message(&self, msg: ConsensusMessage) {
        let peers = self.peers.read().await;
        let network_msg = NetworkMessage {
            msg_type: MessageType::Consensus,
            payload: bincode::serialize(&msg).unwrap(),
            sender: self.node_id.clone(),
        };
        
        // 优先发送给低延迟节点
        let mut sorted_peers: Vec<_> = peers.values().collect();
        sorted_peers.sort_by_key(|p| p.latency_ms);
        
        for peer in sorted_peers {
            let _ = peer.message_queue.send(network_msg.clone()).await;
        }
    }

    pub async fn sync_block(&self, block: Block) {
        let peers = self.peers.read().await;
        let msg = NetworkMessage {
            msg_type: MessageType::Block,
            payload: bincode::serialize(&block).unwrap(),
            sender: self.node_id.clone(),
        };
        
        // 并行发送给多个节点
        let mut handles = Vec::new();
        for peer in peers.values() {
            let queue = peer.message_queue.clone();
            let msg = msg.clone();
            
            let handle = tokio::spawn(async move {
                let _ = queue.send(msg).await;
            });
            handles.push(handle);
        }
        
        for handle in handles {
            let _ = handle.await;
        }
    }

    pub async fn add_peer(&self, peer_id: Vec<u8>, address: String) {
        let (tx, mut rx) = mpsc::channel(1000);
        
        let peer = PeerConnection {
            peer_id: peer_id.clone(),
            address,
            latency_ms: 0,
            bandwidth_mbps: 100,
            message_queue: tx,
        };
        
        self.peers.write().await.insert(peer_id.clone(), peer);
        
        // 启动消息处理协程
        let handler = self.message_handler.clone();
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                Self::handle_message(handler.clone(), msg).await;
            }
        });
    }

    async fn handle_message(handler: Arc<RwLock<MessageHandler>>, msg: NetworkMessage) {
        let handler = handler.read().await;
        
        match msg.msg_type {
            MessageType::Consensus => {
                if let Ok(consensus_msg) = bincode::deserialize::<ConsensusMessage>(&msg.payload) {
                    let _ = handler.consensus_tx.send(consensus_msg).await;
                }
            }
            MessageType::Transaction => {
                if let Ok(tx) = bincode::deserialize::<Transaction>(&msg.payload) {
                    let _ = handler.tx_broadcast_tx.send(tx).await;
                }
            }
            MessageType::Block => {
                if let Ok(block) = bincode::deserialize::<Block>(&msg.payload) {
                    let _ = handler.block_sync_tx.send(block).await;
                }
            }
            _ => {}
        }
    }

    pub async fn measure_peer_latency(&self, peer_id: &[u8]) -> Option<u64> {
        let start = std::time::Instant::now();
        
        // 发送ping消息
        if let Some(peer) = self.peers.read().await.get(peer_id) {
            let ping_msg = NetworkMessage {
                msg_type: MessageType::Ping,
                payload: vec![],
                sender: self.node_id.clone(),
            };
            
            let _ = peer.message_queue.send(ping_msg).await;
            
            // 等待pong响应（简化实现）
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
            
            let latency = start.elapsed().as_millis() as u64;
            
            // 更新延迟信息
            if let Some(mut peer) = self.peers.write().await.get_mut(peer_id) {
                peer.latency_ms = latency;
            }
            
            Some(latency)
        } else {
            None
        }
    }

    pub async fn get_peer_count(&self) -> usize {
        self.peers.read().await.len()
    }

    pub async fn get_tx_pool_size(&self) -> usize {
        self.tx_pool.read().await.pending.len()
    }
}

impl TransactionPool {
    pub fn new() -> Self {
        Self {
            pending: HashMap::new(),
            queued: HashMap::new(),
            max_size: 10000,
            max_per_account: 100,
        }
    }

    pub fn add_transaction(&mut self, tx: Transaction) -> bool {
        if self.pending.len() >= self.max_size {
            return false;
        }
        
        // 检查每个账户的交易数限制
        let account_tx_count = self.pending.values()
            .filter(|t| t.from == tx.from)
            .count();
        
        if account_tx_count >= self.max_per_account {
            // 添加到队列
            self.queued.entry(tx.from)
                .or_insert_with(Vec::new)
                .push(tx);
            return false;
        }
        
        let tx_hash = tx.hash();
        self.pending.insert(tx_hash, tx);
        true
    }

    pub fn get_pending_transactions(&self, limit: usize) -> Vec<Transaction> {
        let mut txs: Vec<_> = self.pending.values().cloned().collect();
        
        // 按gas价格排序（优先级）
        txs.sort_by(|a, b| b.gas_price.cmp(&a.gas_price));
        
        txs.truncate(limit);
        txs
    }

    pub fn remove_transaction(&mut self, hash: &H256) -> Option<Transaction> {
        let tx = self.pending.remove(hash);
        
        // 尝试从队列中提升交易
        if let Some(ref tx) = tx {
            if let Some(queued) = self.queued.get_mut(&tx.from) {
                if let Some(next_tx) = queued.pop() {
                    let next_tx_hash = next_tx.hash();
                    self.pending.insert(next_tx_hash, next_tx);
                }
            }
        }
        
        tx
    }

    pub fn clear(&mut self) {
        self.pending.clear();
        self.queued.clear();
    }

    pub fn prune_old_transactions(&mut self, current_nonce: HashMap<[u8; 20], u64>) {
        self.pending.retain(|_, tx| {
            if let Some(&nonce) = current_nonce.get(&tx.from) {
                tx.nonce >= nonce
            } else {
                true
            }
        });
    }
}