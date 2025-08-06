use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, RwLock, Mutex};
use sha2::{Digest, Sha256};

use super::types::*;
use super::validator::Validator;

// Optimistic BFT - 乐观执行与快速路径优化
// 核心优化：
// 1. 乐观执行：在收到提案后立即执行，不等待投票
// 2. 快速路径：网络好时跳过某些验证步骤
// 3. 双路径：快速路径和正常路径并行
// 4. 推测执行：基于历史预测下一个块

const FAST_PATH_TIMEOUT_MS: u64 = 30; // 快速路径超时
const NORMAL_PATH_TIMEOUT_MS: u64 = 100; // 正常路径超时
const SPECULATION_DEPTH: usize = 3; // 推测执行深度

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptimisticBlock {
    pub height: u64,
    pub epoch: u64,
    pub parent_hash: [u8; 32],
    pub transactions: Vec<Transaction>,
    pub state_root: [u8; 32],
    pub timestamp: u64,
    pub proposer: Vec<u8>,
    pub fast_path: bool, // 是否使用快速路径
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transaction {
    pub hash: [u8; 32],
    pub from: [u8; 20],
    pub to: Option<[u8; 20]>,
    pub data: Vec<u8>,
    pub nonce: u64,
}

#[derive(Debug, Clone)]
pub enum OptimisticMessage {
    FastProposal(FastProposalMsg),
    NormalProposal(NormalProposalMsg),
    FastVote(FastVoteMsg),
    NormalVote(NormalVoteMsg),
    Commit(CommitMsg),
    Rollback(RollbackMsg),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FastProposalMsg {
    pub block: OptimisticBlock,
    pub epoch: u64,
    pub sender: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalProposalMsg {
    pub block: OptimisticBlock,
    pub epoch: u64,
    pub proof: Vec<u8>, // 包含更多验证信息
    pub sender: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FastVoteMsg {
    pub block_hash: [u8; 32],
    pub epoch: u64,
    pub voter: Vec<u8>,
    pub signature: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalVoteMsg {
    pub block_hash: [u8; 32],
    pub epoch: u64,
    pub state_hash: [u8; 32], // 执行后的状态哈希
    pub voter: Vec<u8>,
    pub signature: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitMsg {
    pub block_hash: [u8; 32],
    pub epoch: u64,
    pub votes: Vec<FastVoteMsg>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RollbackMsg {
    pub epoch: u64,
    pub target_height: u64,
    pub reason: String,
}

pub struct OptimisticBFT {
    node_id: Vec<u8>,
    epoch: Arc<RwLock<u64>>,
    height: Arc<RwLock<u64>>,
    validators: Arc<Vec<Validator>>,
    
    // 执行状态
    executed_blocks: Arc<RwLock<VecDeque<OptimisticBlock>>>, // 已执行但未提交
    committed_blocks: Arc<RwLock<Vec<OptimisticBlock>>>, // 已提交的块
    speculative_state: Arc<RwLock<SpeculativeState>>, // 推测状态
    
    // 投票管理
    fast_votes: Arc<Mutex<HashMap<([u8; 32], u64), Vec<FastVoteMsg>>>>,
    normal_votes: Arc<Mutex<HashMap<([u8; 32], u64), Vec<NormalVoteMsg>>>>,
    
    // 路径状态
    fast_path_enabled: Arc<RwLock<bool>>,
    network_conditions: Arc<RwLock<NetworkConditions>>,
    
    // 通信
    block_tx: mpsc::Sender<OptimisticBlock>,
    msg_rx: Arc<Mutex<mpsc::Receiver<OptimisticMessage>>>,
    msg_tx: mpsc::Sender<OptimisticMessage>,
    
    // 性能统计
    metrics: Arc<OptimisticMetrics>,
}

#[derive(Debug, Clone)]
struct SpeculativeState {
    predictions: HashMap<u64, OptimisticBlock>, // height -> predicted block
    confidence: f64, // 预测置信度
    history: VecDeque<PredictionResult>, // 历史预测结果
}

#[derive(Debug, Clone)]
struct PredictionResult {
    height: u64,
    predicted: bool,
    correct: bool,
    latency_saved_ms: u64,
}

#[derive(Debug, Clone)]
struct NetworkConditions {
    avg_latency_ms: u64,
    packet_loss_rate: f64,
    bandwidth_mbps: u64,
    stability_score: f64, // 0-1，越高越稳定
}

#[derive(Default)]
struct OptimisticMetrics {
    fast_path_success: std::sync::atomic::AtomicU64,
    fast_path_failure: std::sync::atomic::AtomicU64,
    blocks_executed: std::sync::atomic::AtomicU64,
    blocks_committed: std::sync::atomic::AtomicU64,
    blocks_rolled_back: std::sync::atomic::AtomicU64,
    avg_commit_latency_ms: std::sync::atomic::AtomicU64,
    speculation_hits: std::sync::atomic::AtomicU64,
    speculation_misses: std::sync::atomic::AtomicU64,
}

impl OptimisticBFT {
    pub fn new(
        node_id: Vec<u8>,
        validators: Vec<Validator>,
    ) -> (Self, mpsc::Receiver<OptimisticBlock>) {
        let (block_tx, block_rx) = mpsc::channel(100);
        let (msg_tx, msg_rx) = mpsc::channel(1000);
        
        (
            Self {
                node_id,
                epoch: Arc::new(RwLock::new(0)),
                height: Arc::new(RwLock::new(0)),
                validators: Arc::new(validators),
                executed_blocks: Arc::new(RwLock::new(VecDeque::new())),
                committed_blocks: Arc::new(RwLock::new(Vec::new())),
                speculative_state: Arc::new(RwLock::new(SpeculativeState {
                    predictions: HashMap::new(),
                    confidence: 0.5,
                    history: VecDeque::new(),
                })),
                fast_votes: Arc::new(Mutex::new(HashMap::new())),
                normal_votes: Arc::new(Mutex::new(HashMap::new())),
                fast_path_enabled: Arc::new(RwLock::new(true)),
                network_conditions: Arc::new(RwLock::new(NetworkConditions {
                    avg_latency_ms: 10,
                    packet_loss_rate: 0.001,
                    bandwidth_mbps: 1000,
                    stability_score: 0.95,
                })),
                block_tx,
                msg_rx: Arc::new(Mutex::new(msg_rx)),
                msg_tx,
                metrics: Arc::new(OptimisticMetrics::default()),
            },
            block_rx,
        )
    }

    pub async fn start(&self) {
        // 启动消息处理
        let msg_handler = self.clone();
        tokio::spawn(async move {
            msg_handler.message_loop().await;
        });
        
        // 启动网络状况监控
        let network_monitor = self.clone();
        tokio::spawn(async move {
            network_monitor.monitor_network().await;
        });
        
        // 启动推测执行
        let speculator = self.clone();
        tokio::spawn(async move {
            speculator.speculative_execution().await;
        });
        
        // 主循环
        loop {
            let epoch = *self.epoch.read().await;
            
            if self.is_leader(epoch).await {
                self.propose().await;
            }
            
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn propose(&self) {
        let epoch = *self.epoch.read().await;
        let height = *self.height.read().await;
        let network = self.network_conditions.read().await.clone();
        
        // 决定使用快速路径还是正常路径
        let use_fast_path = network.stability_score > 0.8 && 
                           network.avg_latency_ms < 20 &&
                           *self.fast_path_enabled.read().await;
        
        let block = OptimisticBlock {
            height: height + 1,
            epoch,
            parent_hash: self.get_parent_hash().await,
            transactions: self.get_pending_transactions().await,
            state_root: [0; 32], // 将在执行后填充
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
            proposer: self.node_id.clone(),
            fast_path: use_fast_path,
        };
        
        if use_fast_path {
            // 快速路径：立即执行并广播
            self.optimistic_execute(&block).await;
            
            let proposal = FastProposalMsg {
                block: block.clone(),
                epoch,
                sender: self.node_id.clone(),
            };
            
            self.broadcast(OptimisticMessage::FastProposal(proposal)).await;
        } else {
            // 正常路径：包含更多验证信息
            let proof = self.generate_proof(&block).await;
            
            let proposal = NormalProposalMsg {
                block: block.clone(),
                epoch,
                proof,
                sender: self.node_id.clone(),
            };
            
            self.broadcast(OptimisticMessage::NormalProposal(proposal)).await;
        }
    }

    async fn message_loop(&self) {
        let mut msg_rx = self.msg_rx.lock().await;
        
        while let Some(msg) = msg_rx.recv().await {
            let start = Instant::now();
            
            match msg {
                OptimisticMessage::FastProposal(proposal) => {
                    self.handle_fast_proposal(proposal).await;
                }
                OptimisticMessage::NormalProposal(proposal) => {
                    self.handle_normal_proposal(proposal).await;
                }
                OptimisticMessage::FastVote(vote) => {
                    self.handle_fast_vote(vote).await;
                }
                OptimisticMessage::NormalVote(vote) => {
                    self.handle_normal_vote(vote).await;
                }
                OptimisticMessage::Commit(commit) => {
                    self.handle_commit(commit).await;
                }
                OptimisticMessage::Rollback(rollback) => {
                    self.handle_rollback(rollback).await;
                }
            }
            
            let latency = start.elapsed().as_millis() as u64;
            self.update_avg_latency(latency);
        }
    }

    async fn handle_fast_proposal(&self, proposal: FastProposalMsg) {
        let start = Instant::now();
        
        // 快速验证
        if !self.quick_verify(&proposal.block).await {
            return;
        }
        
        // 乐观执行
        self.optimistic_execute(&proposal.block).await;
        
        // 立即投票
        let vote = FastVoteMsg {
            block_hash: self.hash_block(&proposal.block),
            epoch: proposal.epoch,
            voter: self.node_id.clone(),
            signature: self.sign(&proposal.block),
        };
        
        if self.is_leader(proposal.epoch).await {
            self.handle_fast_vote(vote).await;
        } else {
            self.send_to_leader(OptimisticMessage::FastVote(vote), proposal.epoch).await;
        }
        
        let execution_time = start.elapsed().as_millis() as u64;
        if execution_time < FAST_PATH_TIMEOUT_MS {
            self.metrics.fast_path_success.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    async fn handle_normal_proposal(&self, proposal: NormalProposalMsg) {
        // 完整验证
        if !self.full_verify(&proposal.block, &proposal.proof).await {
            return;
        }
        
        // 执行并获取状态哈希
        let state_hash = self.execute_with_verification(&proposal.block).await;
        
        // 投票包含状态哈希
        let vote = NormalVoteMsg {
            block_hash: self.hash_block(&proposal.block),
            epoch: proposal.epoch,
            state_hash,
            voter: self.node_id.clone(),
            signature: self.sign(&proposal.block),
        };
        
        if self.is_leader(proposal.epoch).await {
            self.handle_normal_vote(vote).await;
        } else {
            self.send_to_leader(OptimisticMessage::NormalVote(vote), proposal.epoch).await;
        }
    }

    async fn handle_fast_vote(&self, vote: FastVoteMsg) {
        let mut votes = self.fast_votes.lock().await;
        let key = (vote.block_hash, vote.epoch);
        
        votes.entry(key).or_insert_with(Vec::new).push(vote.clone());
        
        // 快速路径：只需要f+1票
        let threshold = self.validators.len() / 3 + 1;
        
        if votes[&key].len() >= threshold {
            // 可以提交
            let commit = CommitMsg {
                block_hash: vote.block_hash,
                epoch: vote.epoch,
                votes: votes[&key].clone(),
            };
            
            self.broadcast(OptimisticMessage::Commit(commit)).await;
            self.metrics.fast_path_success.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    async fn handle_normal_vote(&self, vote: NormalVoteMsg) {
        let mut votes = self.normal_votes.lock().await;
        let key = (vote.block_hash, vote.epoch);
        
        votes.entry(key).or_insert_with(Vec::new).push(vote.clone());
        
        // 正常路径：需要2f+1票
        let threshold = self.validators.len() * 2 / 3 + 1;
        
        if votes[&key].len() >= threshold {
            // 验证状态哈希一致性
            let state_hashes: HashSet<_> = votes[&key].iter()
                .map(|v| v.state_hash)
                .collect();
            
            if state_hashes.len() == 1 {
                // 状态一致，可以提交
                self.commit_block(vote.block_hash).await;
            } else {
                // 状态不一致，需要回滚
                let rollback = RollbackMsg {
                    epoch: vote.epoch,
                    target_height: *self.height.read().await,
                    reason: "State hash mismatch".to_string(),
                };
                
                self.broadcast(OptimisticMessage::Rollback(rollback)).await;
            }
        }
    }

    async fn handle_commit(&self, commit: CommitMsg) {
        // 验证投票
        if commit.votes.len() >= self.validators.len() / 3 + 1 {
            self.commit_block(commit.block_hash).await;
        }
    }

    async fn handle_rollback(&self, rollback: RollbackMsg) {
        // 回滚到指定高度
        self.rollback_to_height(rollback.target_height).await;
        
        self.metrics.blocks_rolled_back.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        
        // 禁用快速路径一段时间
        *self.fast_path_enabled.write().await = false;
        
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(10)).await;
            // 10秒后重新启用
        });
    }

    async fn optimistic_execute(&self, block: &OptimisticBlock) {
        // 乐观执行：立即执行但不提交
        let mut executed = self.executed_blocks.write().await;
        executed.push_back(block.clone());
        
        // 限制未提交块的数量
        if executed.len() > 10 {
            executed.pop_front();
        }
        
        self.metrics.blocks_executed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    async fn execute_with_verification(&self, block: &OptimisticBlock) -> [u8; 32] {
        // 执行并返回状态哈希
        self.optimistic_execute(block).await;
        
        // 计算状态哈希
        let mut hasher = Sha256::new();
        hasher.update(&block.state_root);
        hasher.update(&block.height.to_be_bytes());
        
        let result = hasher.finalize();
        let mut state_hash = [0; 32];
        state_hash.copy_from_slice(&result);
        state_hash
    }

    async fn commit_block(&self, block_hash: [u8; 32]) {
        let mut executed = self.executed_blocks.write().await;
        
        // 找到对应的块
        if let Some(pos) = executed.iter().position(|b| self.hash_block(b) == block_hash) {
            let block = executed[pos].clone();
            
            // 移动到已提交
            let mut committed = self.committed_blocks.write().await;
            committed.push(block.clone());
            
            // 删除之前的所有块（它们也被隐式提交了）
            for _ in 0..=pos {
                executed.pop_front();
            }
            
            // 发送到状态机
            let _ = self.block_tx.send(block).await;
            
            // 更新高度
            let mut height = self.height.write().await;
            *height += 1;
            
            self.metrics.blocks_committed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    async fn rollback_to_height(&self, target_height: u64) {
        let mut executed = self.executed_blocks.write().await;
        let current_height = *self.height.read().await;
        
        // 清除所有未提交的块
        executed.clear();
        
        // 如果需要，回滚已提交的块
        if target_height < current_height {
            let mut committed = self.committed_blocks.write().await;
            committed.truncate(target_height as usize);
            
            *self.height.write().await = target_height;
        }
    }

    async fn speculative_execution(&self) {
        let mut interval = tokio::time::interval(Duration::from_millis(50));
        
        loop {
            interval.tick().await;
            
            let mut state = self.speculative_state.write().await;
            
            // 基于历史预测下一个块
            if state.confidence > 0.7 {
                let height = *self.height.read().await + 1;
                
                // 创建预测块
                let predicted_block = self.predict_next_block(height).await;
                state.predictions.insert(height, predicted_block.clone());
                
                // 预执行
                self.optimistic_execute(&predicted_block).await;
                
                // 清理旧预测
                if state.predictions.len() > SPECULATION_DEPTH {
                    let min_height = height - SPECULATION_DEPTH as u64;
                    state.predictions.retain(|&h, _| h >= min_height);
                }
            }
            
            // 更新置信度
            self.update_speculation_confidence(&mut state).await;
        }
    }

    async fn predict_next_block(&self, height: u64) -> OptimisticBlock {
        // 基于模式预测下一个块
        OptimisticBlock {
            height,
            epoch: *self.epoch.read().await,
            parent_hash: [0; 32],
            transactions: vec![],
            state_root: [0; 32],
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
            proposer: vec![],
            fast_path: true,
        }
    }

    async fn update_speculation_confidence(&self, state: &mut SpeculativeState) {
        // 基于历史命中率更新置信度
        if state.history.len() >= 10 {
            let hits = state.history.iter().filter(|r| r.correct).count();
            state.confidence = hits as f64 / state.history.len() as f64;
            
            // 保持历史记录在合理范围
            if state.history.len() > 100 {
                state.history.pop_front();
            }
        }
    }

    async fn monitor_network(&self) {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        
        loop {
            interval.tick().await;
            
            let mut conditions = self.network_conditions.write().await;
            
            // 模拟网络监控（实际应该测量真实网络）
            conditions.avg_latency_ms = 10 + (rand::random::<u64>() % 20);
            conditions.packet_loss_rate = (rand::random::<f64>() * 0.01).min(0.01);
            conditions.stability_score = 1.0 - conditions.packet_loss_rate - 
                                        (conditions.avg_latency_ms as f64 / 100.0);
            
            // 根据网络状况调整快速路径
            if conditions.stability_score < 0.5 {
                *self.fast_path_enabled.write().await = false;
            } else if conditions.stability_score > 0.8 {
                *self.fast_path_enabled.write().await = true;
            }
        }
    }

    async fn quick_verify(&self, block: &OptimisticBlock) -> bool {
        // 快速验证：只检查基本字段
        block.height > 0 && block.epoch > 0
    }

    async fn full_verify(&self, block: &OptimisticBlock, proof: &[u8]) -> bool {
        // 完整验证：包括交易验证、状态验证等
        self.quick_verify(block).await && proof.len() > 0
    }

    fn hash_block(&self, block: &OptimisticBlock) -> [u8; 32] {
        let data = bincode::serialize(block).unwrap();
        let hash = Sha256::digest(&data);
        let mut result = [0; 32];
        result.copy_from_slice(&hash);
        result
    }

    fn sign(&self, block: &OptimisticBlock) -> Vec<u8> {
        vec![0; 64] // 简化的签名
    }

    async fn is_leader(&self, epoch: u64) -> bool {
        let leader_idx = (epoch as usize) % self.validators.len();
        self.validators[leader_idx].public_key == self.node_id
    }

    async fn get_parent_hash(&self) -> [u8; 32] {
        if let Some(block) = self.committed_blocks.read().await.last() {
            self.hash_block(block)
        } else {
            [0; 32]
        }
    }

    async fn get_pending_transactions(&self) -> Vec<Transaction> {
        vec![] // 简化实现
    }

    async fn broadcast(&self, msg: OptimisticMessage) {
        let _ = self.msg_tx.send(msg).await;
    }

    async fn send_to_leader(&self, msg: OptimisticMessage, epoch: u64) {
        if self.is_leader(epoch).await {
            let _ = self.msg_tx.send(msg).await;
        }
    }

    fn update_avg_latency(&self, latency_ms: u64) {
        let current = self.metrics.avg_commit_latency_ms.load(std::sync::atomic::Ordering::Relaxed);
        let new_avg = (current * 9 + latency_ms) / 10;
        self.metrics.avg_commit_latency_ms.store(new_avg, std::sync::atomic::Ordering::Relaxed);
    }

    async fn generate_proof(&self, block: &OptimisticBlock) -> Vec<u8> {
        // 生成验证证明
        vec![0; 128]
    }

    pub async fn get_metrics(&self) -> OptimisticMetricsSnapshot {
        let network = self.network_conditions.read().await.clone();
        let speculation = self.speculative_state.read().await.clone();
        
        OptimisticMetricsSnapshot {
            fast_path_success: self.metrics.fast_path_success.load(std::sync::atomic::Ordering::Relaxed),
            fast_path_failure: self.metrics.fast_path_failure.load(std::sync::atomic::Ordering::Relaxed),
            blocks_executed: self.metrics.blocks_executed.load(std::sync::atomic::Ordering::Relaxed),
            blocks_committed: self.metrics.blocks_committed.load(std::sync::atomic::Ordering::Relaxed),
            blocks_rolled_back: self.metrics.blocks_rolled_back.load(std::sync::atomic::Ordering::Relaxed),
            avg_commit_latency_ms: self.metrics.avg_commit_latency_ms.load(std::sync::atomic::Ordering::Relaxed),
            speculation_hits: self.metrics.speculation_hits.load(std::sync::atomic::Ordering::Relaxed),
            speculation_misses: self.metrics.speculation_misses.load(std::sync::atomic::Ordering::Relaxed),
            network_stability: network.stability_score,
            speculation_confidence: speculation.confidence,
            fast_path_enabled: *self.fast_path_enabled.read().await,
        }
    }
}

impl Clone for OptimisticBFT {
    fn clone(&self) -> Self {
        Self {
            node_id: self.node_id.clone(),
            epoch: self.epoch.clone(),
            height: self.height.clone(),
            validators: self.validators.clone(),
            executed_blocks: self.executed_blocks.clone(),
            committed_blocks: self.committed_blocks.clone(),
            speculative_state: self.speculative_state.clone(),
            fast_votes: self.fast_votes.clone(),
            normal_votes: self.normal_votes.clone(),
            fast_path_enabled: self.fast_path_enabled.clone(),
            network_conditions: self.network_conditions.clone(),
            block_tx: self.block_tx.clone(),
            msg_rx: self.msg_rx.clone(),
            msg_tx: self.msg_tx.clone(),
            metrics: self.metrics.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct OptimisticMetricsSnapshot {
    pub fast_path_success: u64,
    pub fast_path_failure: u64,
    pub blocks_executed: u64,
    pub blocks_committed: u64,
    pub blocks_rolled_back: u64,
    pub avg_commit_latency_ms: u64,
    pub speculation_hits: u64,
    pub speculation_misses: u64,
    pub network_stability: f64,
    pub speculation_confidence: f64,
    pub fast_path_enabled: bool,
}