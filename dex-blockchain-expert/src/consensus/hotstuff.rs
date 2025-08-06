use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, RwLock, Mutex};
use sha2::{Digest, Sha256};

use super::types::*;
use super::validator::Validator;

// HotStuff共识算法实现 - 线性消息复杂度O(n)
// 主要优化：
// 1. 链式HotStuff - 将多个阶段合并
// 2. 流水线处理 - 并行处理多个视图
// 3. 聚合签名 - 减少消息大小

const VIEW_TIMEOUT_MS: u64 = 50; // 更短的超时时间
const DELTA_MS: u64 = 10; // 网络延迟估计

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotStuffBlock {
    pub height: u64,
    pub view: u64,
    pub parent_hash: [u8; 32],
    pub qc: QuorumCertificate, // 包含前一个块的QC
    pub transactions: Vec<Transaction>,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuorumCertificate {
    pub block_hash: [u8; 32],
    pub view: u64,
    pub signatures: Vec<PartialSignature>, // 聚合签名
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartialSignature {
    pub validator_id: Vec<u8>,
    pub signature: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transaction {
    pub hash: [u8; 32],
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub enum HotStuffMessage {
    Proposal(ProposalMsg),
    Vote(VoteMsg),
    NewView(NewViewMsg),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposalMsg {
    pub block: HotStuffBlock,
    pub justify: QuorumCertificate, // 上一个QC
    pub view: u64,
    pub sender: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoteMsg {
    pub block_hash: [u8; 32],
    pub view: u64,
    pub partial_sig: PartialSignature,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewViewMsg {
    pub view: u64,
    pub justify: QuorumCertificate,
    pub sender: Vec<u8>,
}

pub struct HotStuffConsensus {
    node_id: Vec<u8>,
    view: Arc<RwLock<u64>>,
    height: Arc<RwLock<u64>>,
    validators: Arc<Vec<Validator>>,
    
    // 三链结构
    b_lock: Arc<RwLock<Option<HotStuffBlock>>>, // locked block
    b_exec: Arc<RwLock<Option<HotStuffBlock>>>, // executed block
    b_leaf: Arc<RwLock<Option<HotStuffBlock>>>, // leaf block
    
    // QC管理
    generic_qc: Arc<RwLock<Option<QuorumCertificate>>>,
    locked_qc: Arc<RwLock<Option<QuorumCertificate>>>,
    highest_qc: Arc<RwLock<Option<QuorumCertificate>>>,
    
    // 投票收集
    votes: Arc<Mutex<HashMap<([u8; 32], u64), Vec<VoteMsg>>>>,
    
    // 通信通道
    block_tx: mpsc::Sender<HotStuffBlock>,
    msg_rx: Arc<Mutex<mpsc::Receiver<HotStuffMessage>>>,
    msg_tx: mpsc::Sender<HotStuffMessage>,
    
    // 性能统计
    metrics: Arc<HotStuffMetrics>,
}

#[derive(Default)]
struct HotStuffMetrics {
    views_processed: std::sync::atomic::AtomicU64,
    blocks_committed: std::sync::atomic::AtomicU64,
    avg_latency_ms: std::sync::atomic::AtomicU64,
    message_count: std::sync::atomic::AtomicU64,
}

impl HotStuffConsensus {
    pub fn new(
        node_id: Vec<u8>,
        validators: Vec<Validator>,
    ) -> (Self, mpsc::Receiver<HotStuffBlock>) {
        let (block_tx, block_rx) = mpsc::channel(100);
        let (msg_tx, msg_rx) = mpsc::channel(1000);
        
        (
            Self {
                node_id,
                view: Arc::new(RwLock::new(0)),
                height: Arc::new(RwLock::new(0)),
                validators: Arc::new(validators),
                b_lock: Arc::new(RwLock::new(None)),
                b_exec: Arc::new(RwLock::new(None)),
                b_leaf: Arc::new(RwLock::new(None)),
                generic_qc: Arc::new(RwLock::new(None)),
                locked_qc: Arc::new(RwLock::new(None)),
                highest_qc: Arc::new(RwLock::new(None)),
                votes: Arc::new(Mutex::new(HashMap::new())),
                block_tx,
                msg_rx: Arc::new(Mutex::new(msg_rx)),
                msg_tx,
                metrics: Arc::new(HotStuffMetrics::default()),
            },
            block_rx,
        )
    }

    pub async fn start(&self) {
        // 启动消息处理循环
        let msg_handler = self.clone();
        tokio::spawn(async move {
            msg_handler.message_loop().await;
        });
        
        // 启动视图变更定时器
        let view_timer = self.clone();
        tokio::spawn(async move {
            view_timer.view_change_timer().await;
        });
        
        // 主循环
        loop {
            let view = *self.view.read().await;
            
            if self.is_leader(view).await {
                self.propose().await;
            }
            
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn message_loop(&self) {
        let mut msg_rx = self.msg_rx.lock().await;
        
        while let Some(msg) = msg_rx.recv().await {
            let start = Instant::now();
            
            match msg {
                HotStuffMessage::Proposal(proposal) => {
                    self.handle_proposal(proposal).await;
                }
                HotStuffMessage::Vote(vote) => {
                    self.handle_vote(vote).await;
                }
                HotStuffMessage::NewView(new_view) => {
                    self.handle_new_view(new_view).await;
                }
            }
            
            // 更新延迟统计
            let latency = start.elapsed().as_millis() as u64;
            self.update_avg_latency(latency);
            
            self.metrics.message_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    async fn propose(&self) {
        let view = *self.view.read().await;
        let height = *self.height.read().await;
        
        // 创建新块
        let parent = self.b_leaf.read().await.clone();
        let parent_hash = parent.as_ref()
            .map(|b| self.hash_block(b))
            .unwrap_or([0; 32]);
        
        let highest_qc = self.highest_qc.read().await.clone()
            .unwrap_or_else(|| self.genesis_qc());
        
        let block = HotStuffBlock {
            height: height + 1,
            view,
            parent_hash,
            qc: highest_qc.clone(),
            transactions: self.get_pending_transactions().await,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
        };
        
        let proposal = ProposalMsg {
            block: block.clone(),
            justify: highest_qc,
            view,
            sender: self.node_id.clone(),
        };
        
        // 广播提案
        self.broadcast(HotStuffMessage::Proposal(proposal)).await;
        
        // 自己投票
        self.vote_for_block(&block).await;
    }

    async fn handle_proposal(&self, proposal: ProposalMsg) {
        let view = *self.view.read().await;
        
        // 验证视图号
        if proposal.view != view {
            return;
        }
        
        // 验证QC
        if !self.verify_qc(&proposal.justify).await {
            return;
        }
        
        // 验证块
        if !self.verify_block(&proposal.block).await {
            return;
        }
        
        // 更新状态
        self.update(proposal.block.clone(), proposal.justify.clone()).await;
        
        // 投票
        self.vote_for_block(&proposal.block).await;
    }

    async fn vote_for_block(&self, block: &HotStuffBlock) {
        let block_hash = self.hash_block(block);
        
        // 创建部分签名
        let partial_sig = PartialSignature {
            validator_id: self.node_id.clone(),
            signature: self.sign(&block_hash),
        };
        
        let vote = VoteMsg {
            block_hash,
            view: block.view,
            partial_sig,
        };
        
        // 如果是领导者，收集自己的投票
        if self.is_leader(block.view).await {
            self.handle_vote(vote.clone()).await;
        } else {
            // 发送给领导者
            self.send_to_leader(HotStuffMessage::Vote(vote), block.view).await;
        }
    }

    async fn handle_vote(&self, vote: VoteMsg) {
        let mut votes = self.votes.lock().await;
        let key = (vote.block_hash, vote.view);
        
        votes.entry(key).or_insert_with(Vec::new).push(vote.clone());
        
        // 检查是否收集到足够的投票 (2f+1)
        let threshold = self.validators.len() * 2 / 3 + 1;
        
        if votes[&key].len() >= threshold {
            // 创建QC
            let qc = QuorumCertificate {
                block_hash: vote.block_hash,
                view: vote.view,
                signatures: votes[&key].iter()
                    .map(|v| v.partial_sig.clone())
                    .collect(),
            };
            
            // 更新highest_qc
            let mut highest_qc = self.highest_qc.write().await;
            *highest_qc = Some(qc.clone());
            
            // 进入下一个视图
            self.enter_new_view(vote.view + 1).await;
        }
    }

    async fn update(&self, block: HotStuffBlock, qc: QuorumCertificate) {
        // 更新b_leaf
        let mut b_leaf = self.b_leaf.write().await;
        *b_leaf = Some(block.clone());
        
        // 检查是否可以更新b_lock (2-chain)
        if let Some(parent) = self.get_block(&block.parent_hash).await {
            if qc.block_hash == self.hash_block(&parent) {
                let mut b_lock = self.b_lock.write().await;
                *b_lock = Some(parent.clone());
                
                // 检查是否可以提交 (3-chain)
                if let Some(grandparent) = self.get_block(&parent.parent_hash).await {
                    if parent.qc.block_hash == self.hash_block(&grandparent) {
                        self.commit_block(grandparent).await;
                    }
                }
            }
        }
    }

    async fn commit_block(&self, block: HotStuffBlock) {
        // 执行块
        let mut b_exec = self.b_exec.write().await;
        *b_exec = Some(block.clone());
        
        // 发送到执行层
        let _ = self.block_tx.send(block).await;
        
        // 更新高度
        let mut height = self.height.write().await;
        *height += 1;
        
        self.metrics.blocks_committed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    async fn enter_new_view(&self, new_view: u64) {
        let mut view = self.view.write().await;
        *view = new_view;
        
        // 发送NewView消息
        if let Some(qc) = self.highest_qc.read().await.clone() {
            let msg = NewViewMsg {
                view: new_view,
                justify: qc,
                sender: self.node_id.clone(),
            };
            
            if self.is_leader(new_view).await {
                self.handle_new_view(msg).await;
            } else {
                self.send_to_leader(HotStuffMessage::NewView(msg), new_view).await;
            }
        }
        
        self.metrics.views_processed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    async fn handle_new_view(&self, msg: NewViewMsg) {
        // 收集NewView消息，当收到2f+1个时，开始新的提案
        // 简化实现：直接进入提案阶段
        if self.is_leader(msg.view).await {
            self.propose().await;
        }
    }

    async fn view_change_timer(&self) {
        let mut interval = tokio::time::interval(Duration::from_millis(VIEW_TIMEOUT_MS));
        let mut last_view = 0;
        
        loop {
            interval.tick().await;
            
            let current_view = *self.view.read().await;
            
            // 如果视图没有变化，触发超时
            if current_view == last_view {
                self.enter_new_view(current_view + 1).await;
            }
            
            last_view = current_view;
        }
    }

    async fn is_leader(&self, view: u64) -> bool {
        let leader_idx = (view as usize) % self.validators.len();
        self.validators[leader_idx].public_key == self.node_id
    }

    async fn get_block(&self, hash: &[u8; 32]) -> Option<HotStuffBlock> {
        // 简化实现：检查当前持有的块
        if let Some(block) = self.b_leaf.read().await.as_ref() {
            if self.hash_block(block) == *hash {
                return Some(block.clone());
            }
        }
        
        if let Some(block) = self.b_lock.read().await.as_ref() {
            if self.hash_block(block) == *hash {
                return Some(block.clone());
            }
        }
        
        None
    }

    fn hash_block(&self, block: &HotStuffBlock) -> [u8; 32] {
        let data = bincode::serialize(block).unwrap();
        let hash = Sha256::digest(&data);
        let mut result = [0; 32];
        result.copy_from_slice(&hash);
        result
    }

    fn sign(&self, data: &[u8; 32]) -> Vec<u8> {
        // 简化的签名实现
        let mut sig = vec![0u8; 64];
        sig[..32].copy_from_slice(data);
        sig[32..36].copy_from_slice(&self.node_id[..4.min(self.node_id.len())]);
        sig
    }

    async fn verify_qc(&self, qc: &QuorumCertificate) -> bool {
        // 验证签名数量
        let threshold = self.validators.len() * 2 / 3 + 1;
        qc.signatures.len() >= threshold
    }

    async fn verify_block(&self, block: &HotStuffBlock) -> bool {
        // 基本验证
        block.height > 0 && block.view >= 0
    }

    async fn broadcast(&self, msg: HotStuffMessage) {
        // 简化实现：发送给自己处理
        let _ = self.msg_tx.send(msg).await;
    }

    async fn send_to_leader(&self, msg: HotStuffMessage, view: u64) {
        // 简化实现：发送给自己处理
        if self.is_leader(view).await {
            let _ = self.msg_tx.send(msg).await;
        }
    }

    async fn get_pending_transactions(&self) -> Vec<Transaction> {
        // 获取待处理的交易
        vec![]
    }

    fn genesis_qc(&self) -> QuorumCertificate {
        QuorumCertificate {
            block_hash: [0; 32],
            view: 0,
            signatures: vec![],
        }
    }

    fn update_avg_latency(&self, latency_ms: u64) {
        let current = self.metrics.avg_latency_ms.load(std::sync::atomic::Ordering::Relaxed);
        let new_avg = (current * 9 + latency_ms) / 10; // 移动平均
        self.metrics.avg_latency_ms.store(new_avg, std::sync::atomic::Ordering::Relaxed);
    }

    pub async fn get_metrics(&self) -> HotStuffMetricsSnapshot {
        HotStuffMetricsSnapshot {
            views_processed: self.metrics.views_processed.load(std::sync::atomic::Ordering::Relaxed),
            blocks_committed: self.metrics.blocks_committed.load(std::sync::atomic::Ordering::Relaxed),
            avg_latency_ms: self.metrics.avg_latency_ms.load(std::sync::atomic::Ordering::Relaxed),
            message_count: self.metrics.message_count.load(std::sync::atomic::Ordering::Relaxed),
            current_view: *self.view.read().await,
            current_height: *self.height.read().await,
        }
    }
}

impl Clone for HotStuffConsensus {
    fn clone(&self) -> Self {
        Self {
            node_id: self.node_id.clone(),
            view: self.view.clone(),
            height: self.height.clone(),
            validators: self.validators.clone(),
            b_lock: self.b_lock.clone(),
            b_exec: self.b_exec.clone(),
            b_leaf: self.b_leaf.clone(),
            generic_qc: self.generic_qc.clone(),
            locked_qc: self.locked_qc.clone(),
            highest_qc: self.highest_qc.clone(),
            votes: self.votes.clone(),
            block_tx: self.block_tx.clone(),
            msg_rx: self.msg_rx.clone(),
            msg_tx: self.msg_tx.clone(),
            metrics: self.metrics.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct HotStuffMetricsSnapshot {
    pub views_processed: u64,
    pub blocks_committed: u64,
    pub avg_latency_ms: u64,
    pub message_count: u64,
    pub current_view: u64,
    pub current_height: u64,
}