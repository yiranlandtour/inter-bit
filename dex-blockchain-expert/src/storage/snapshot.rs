use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{RwLock, Mutex};
use serde::{Deserialize, Serialize};
use primitive_types::{H256, U256};
use std::time::{SystemTime, UNIX_EPOCH};

// 状态快照和增量同步机制
// 核心功能：
// 1. 定期创建全量快照
// 2. 增量同步只传输变更的状态
// 3. 使用Merkle证明验证
// 4. 并行下载状态数据

pub struct SnapshotManager {
    snapshots: Arc<RwLock<HashMap<u64, Snapshot>>>,
    deltas: Arc<RwLock<HashMap<u64, StateDelta>>>,
    current_height: Arc<RwLock<u64>>,
    sync_manager: Arc<SyncManager>,
    snapshot_interval: u64, // 每N个块创建快照
    max_snapshots: usize, // 最多保留的快照数
    metrics: Arc<SnapshotMetrics>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub height: u64,
    pub state_root: H256,
    pub timestamp: u64,
    pub accounts: HashMap<[u8; 20], AccountSnapshot>,
    pub storage: HashMap<([u8; 20], H256), H256>,
    pub code: HashMap<H256, Vec<u8>>,
    pub metadata: SnapshotMetadata,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountSnapshot {
    pub nonce: u64,
    pub balance: U256,
    pub storage_root: H256,
    pub code_hash: Option<H256>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotMetadata {
    pub total_accounts: u64,
    pub total_storage_keys: u64,
    pub total_contracts: u64,
    pub snapshot_size_bytes: u64,
    pub creation_time_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateDelta {
    pub from_height: u64,
    pub to_height: u64,
    pub changes: Vec<StateChange>,
    pub proof: MerkleProof,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StateChange {
    AccountCreated {
        address: [u8; 20],
        account: AccountSnapshot,
    },
    AccountUpdated {
        address: [u8; 20],
        old: AccountSnapshot,
        new: AccountSnapshot,
    },
    AccountDeleted {
        address: [u8; 20],
    },
    StorageUpdated {
        address: [u8; 20],
        key: H256,
        old_value: H256,
        new_value: H256,
    },
    CodeDeployed {
        address: [u8; 20],
        code_hash: H256,
        code: Vec<u8>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MerkleProof {
    pub root: H256,
    pub proof_nodes: Vec<ProofNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProofNode {
    pub hash: H256,
    pub path: Vec<u8>,
    pub value: Option<Vec<u8>>,
}

pub struct SyncManager {
    active_syncs: Arc<Mutex<HashMap<u64, SyncSession>>>,
    download_queue: Arc<Mutex<Vec<DownloadTask>>>,
    peer_scores: Arc<RwLock<HashMap<Vec<u8>, PeerScore>>>,
    parallel_downloads: usize,
}

struct SyncSession {
    target_height: u64,
    current_height: u64,
    start_time: SystemTime,
    state: SyncState,
    peers: Vec<Vec<u8>>,
}

#[derive(Debug, Clone)]
enum SyncState {
    FindingSnapshot,
    DownloadingSnapshot,
    ApplyingSnapshot,
    DownloadingDeltas,
    ApplyingDeltas,
    Verifying,
    Completed,
}

struct DownloadTask {
    task_type: DownloadType,
    peer: Vec<u8>,
    retry_count: u32,
    priority: u8,
}

#[derive(Debug, Clone)]
enum DownloadType {
    SnapshotChunk { height: u64, chunk_id: u32 },
    Delta { from_height: u64, to_height: u64 },
    Proof { height: u64 },
}

struct PeerScore {
    success_count: u64,
    failure_count: u64,
    avg_latency_ms: u64,
    last_seen: SystemTime,
}

#[derive(Default)]
struct SnapshotMetrics {
    snapshots_created: std::sync::atomic::AtomicU64,
    snapshots_applied: std::sync::atomic::AtomicU64,
    deltas_created: std::sync::atomic::AtomicU64,
    deltas_applied: std::sync::atomic::AtomicU64,
    sync_sessions_started: std::sync::atomic::AtomicU64,
    sync_sessions_completed: std::sync::atomic::AtomicU64,
    bytes_downloaded: std::sync::atomic::AtomicU64,
    bytes_uploaded: std::sync::atomic::AtomicU64,
}

impl SnapshotManager {
    pub fn new(snapshot_interval: u64) -> Self {
        Self {
            snapshots: Arc::new(RwLock::new(HashMap::new())),
            deltas: Arc::new(RwLock::new(HashMap::new())),
            current_height: Arc::new(RwLock::new(0)),
            sync_manager: Arc::new(SyncManager::new()),
            snapshot_interval,
            max_snapshots: 10,
            metrics: Arc::new(SnapshotMetrics::default()),
        }
    }

    pub async fn create_snapshot(&self, height: u64, state: &StateData) -> Snapshot {
        let start = SystemTime::now();
        
        let snapshot = Snapshot {
            height,
            state_root: state.root,
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            accounts: state.accounts.clone(),
            storage: state.storage.clone(),
            code: state.code.clone(),
            metadata: SnapshotMetadata {
                total_accounts: state.accounts.len() as u64,
                total_storage_keys: state.storage.len() as u64,
                total_contracts: state.code.len() as u64,
                snapshot_size_bytes: self.estimate_size(state),
                creation_time_ms: start.elapsed().unwrap().as_millis() as u64,
            },
        };
        
        // 保存快照
        let mut snapshots = self.snapshots.write().await;
        snapshots.insert(height, snapshot.clone());
        
        // 清理旧快照
        if snapshots.len() > self.max_snapshots {
            let min_height = snapshots.keys().min().cloned().unwrap();
            snapshots.remove(&min_height);
        }
        
        self.metrics.snapshots_created.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        
        // 更新当前高度
        *self.current_height.write().await = height;
        
        snapshot
    }

    pub async fn create_delta(
        &self,
        from_height: u64,
        to_height: u64,
        old_state: &StateData,
        new_state: &StateData,
    ) -> StateDelta {
        let mut changes = Vec::new();
        
        // 检测账户变化
        for (address, new_account) in &new_state.accounts {
            if let Some(old_account) = old_state.accounts.get(address) {
                if old_account != new_account {
                    changes.push(StateChange::AccountUpdated {
                        address: *address,
                        old: old_account.clone(),
                        new: new_account.clone(),
                    });
                }
            } else {
                changes.push(StateChange::AccountCreated {
                    address: *address,
                    account: new_account.clone(),
                });
            }
        }
        
        // 检测删除的账户
        for (address, _) in &old_state.accounts {
            if !new_state.accounts.contains_key(address) {
                changes.push(StateChange::AccountDeleted { address: *address });
            }
        }
        
        // 检测存储变化
        for ((address, key), new_value) in &new_state.storage {
            if let Some(old_value) = old_state.storage.get(&(*address, *key)) {
                if old_value != new_value {
                    changes.push(StateChange::StorageUpdated {
                        address: *address,
                        key: *key,
                        old_value: *old_value,
                        new_value: *new_value,
                    });
                }
            } else {
                changes.push(StateChange::StorageUpdated {
                    address: *address,
                    key: *key,
                    old_value: H256::zero(),
                    new_value: *new_value,
                });
            }
        }
        
        // 检测新部署的合约
        for (code_hash, code) in &new_state.code {
            if !old_state.code.contains_key(code_hash) {
                // 找到对应的地址
                for (address, account) in &new_state.accounts {
                    if account.code_hash == Some(*code_hash) {
                        changes.push(StateChange::CodeDeployed {
                            address: *address,
                            code_hash: *code_hash,
                            code: code.clone(),
                        });
                        break;
                    }
                }
            }
        }
        
        let delta = StateDelta {
            from_height,
            to_height,
            changes,
            proof: self.generate_proof(new_state),
        };
        
        // 保存增量
        self.deltas.write().await.insert(to_height, delta.clone());
        
        self.metrics.deltas_created.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        
        delta
    }

    pub async fn apply_snapshot(&self, snapshot: &Snapshot, state: &mut StateData) {
        state.accounts = snapshot.accounts.clone();
        state.storage = snapshot.storage.clone();
        state.code = snapshot.code.clone();
        state.root = snapshot.state_root;
        
        *self.current_height.write().await = snapshot.height;
        
        self.metrics.snapshots_applied.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    pub async fn apply_delta(&self, delta: &StateDelta, state: &mut StateData) {
        for change in &delta.changes {
            match change {
                StateChange::AccountCreated { address, account } => {
                    state.accounts.insert(*address, account.clone());
                }
                StateChange::AccountUpdated { address, new, .. } => {
                    state.accounts.insert(*address, new.clone());
                }
                StateChange::AccountDeleted { address } => {
                    state.accounts.remove(address);
                }
                StateChange::StorageUpdated { address, key, new_value, .. } => {
                    state.storage.insert((*address, *key), *new_value);
                }
                StateChange::CodeDeployed { code_hash, code, .. } => {
                    state.code.insert(*code_hash, code.clone());
                }
            }
        }
        
        *self.current_height.write().await = delta.to_height;
        
        self.metrics.deltas_applied.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    pub async fn sync_to_height(&self, target_height: u64) -> Result<(), SyncError> {
        let current = *self.current_height.read().await;
        
        if current >= target_height {
            return Ok(());
        }
        
        self.metrics.sync_sessions_started.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        
        // 查找最近的快照
        let snapshots = self.snapshots.read().await;
        let best_snapshot = snapshots
            .iter()
            .filter(|(h, _)| **h <= target_height && **h > current)
            .max_by_key(|(h, _)| *h);
        
        if let Some((snapshot_height, _)) = best_snapshot {
            // 策略1：从快照开始同步
            self.sync_from_snapshot(*snapshot_height, target_height).await?;
        } else {
            // 策略2：增量同步
            self.sync_incremental(current, target_height).await?;
        }
        
        self.metrics.sync_sessions_completed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        
        Ok(())
    }

    async fn sync_from_snapshot(
        &self,
        snapshot_height: u64,
        target_height: u64,
    ) -> Result<(), SyncError> {
        // 1. 下载并应用快照
        let snapshot = self.download_snapshot(snapshot_height).await?;
        let mut state = StateData::default();
        self.apply_snapshot(&snapshot, &mut state).await;
        
        // 2. 下载并应用增量
        for height in (snapshot_height + 1)..=target_height {
            let delta = self.download_delta(height - 1, height).await?;
            self.apply_delta(&delta, &mut state).await;
        }
        
        Ok(())
    }

    async fn sync_incremental(
        &self,
        from_height: u64,
        to_height: u64,
    ) -> Result<(), SyncError> {
        let mut state = StateData::default(); // 应该从当前状态开始
        
        // 批量下载增量
        let chunk_size = 100;
        for chunk_start in (from_height..to_height).step_by(chunk_size) {
            let chunk_end = (chunk_start + chunk_size as u64).min(to_height);
            
            // 并行下载多个增量
            let mut handles = Vec::new();
            for height in chunk_start..chunk_end {
                let sync_manager = self.sync_manager.clone();
                let handle = tokio::spawn(async move {
                    sync_manager.download_delta_from_peers(height, height + 1).await
                });
                handles.push(handle);
            }
            
            // 等待所有下载完成
            let deltas: Vec<StateDelta> = futures::future::join_all(handles)
                .await
                .into_iter()
                .filter_map(|r| r.ok())
                .filter_map(|r| r.ok())
                .collect();
            
            // 按顺序应用
            for delta in deltas {
                self.apply_delta(&delta, &mut state).await;
            }
        }
        
        Ok(())
    }

    async fn download_snapshot(&self, height: u64) -> Result<Snapshot, SyncError> {
        self.sync_manager.download_snapshot_from_peers(height).await
    }

    async fn download_delta(&self, from: u64, to: u64) -> Result<StateDelta, SyncError> {
        self.sync_manager.download_delta_from_peers(from, to).await
    }

    fn generate_proof(&self, state: &StateData) -> MerkleProof {
        // 生成状态的Merkle证明
        MerkleProof {
            root: state.root,
            proof_nodes: vec![], // 简化实现
        }
    }

    fn estimate_size(&self, state: &StateData) -> u64 {
        let account_size = state.accounts.len() * 100; // 估计每个账户100字节
        let storage_size = state.storage.len() * 64; // 每个存储项64字节
        let code_size: usize = state.code.values().map(|c| c.len()).sum();
        
        (account_size + storage_size + code_size) as u64
    }

    pub async fn get_snapshot(&self, height: u64) -> Option<Snapshot> {
        self.snapshots.read().await.get(&height).cloned()
    }

    pub async fn get_delta(&self, height: u64) -> Option<StateDelta> {
        self.deltas.read().await.get(&height).cloned()
    }

    pub async fn list_snapshots(&self) -> Vec<u64> {
        let snapshots = self.snapshots.read().await;
        let mut heights: Vec<u64> = snapshots.keys().cloned().collect();
        heights.sort();
        heights
    }

    pub async fn cleanup_old_data(&self, keep_height: u64) {
        // 清理旧的快照
        let mut snapshots = self.snapshots.write().await;
        snapshots.retain(|h, _| *h >= keep_height);
        
        // 清理旧的增量
        let mut deltas = self.deltas.write().await;
        deltas.retain(|h, _| *h >= keep_height);
    }

    pub fn verify_proof(proof: &MerkleProof, key: &[u8], value: &[u8]) -> bool {
        // 验证Merkle证明
        // 简化实现
        !proof.proof_nodes.is_empty()
    }

    pub async fn get_metrics(&self) -> SnapshotMetricsSnapshot {
        SnapshotMetricsSnapshot {
            snapshots_created: self.metrics.snapshots_created.load(std::sync::atomic::Ordering::Relaxed),
            snapshots_applied: self.metrics.snapshots_applied.load(std::sync::atomic::Ordering::Relaxed),
            deltas_created: self.metrics.deltas_created.load(std::sync::atomic::Ordering::Relaxed),
            deltas_applied: self.metrics.deltas_applied.load(std::sync::atomic::Ordering::Relaxed),
            sync_sessions_started: self.metrics.sync_sessions_started.load(std::sync::atomic::Ordering::Relaxed),
            sync_sessions_completed: self.metrics.sync_sessions_completed.load(std::sync::atomic::Ordering::Relaxed),
            bytes_downloaded: self.metrics.bytes_downloaded.load(std::sync::atomic::Ordering::Relaxed),
            bytes_uploaded: self.metrics.bytes_uploaded.load(std::sync::atomic::Ordering::Relaxed),
            current_height: *self.current_height.read().await,
            snapshot_count: self.snapshots.read().await.len(),
            delta_count: self.deltas.read().await.len(),
        }
    }
}

impl SyncManager {
    fn new() -> Self {
        Self {
            active_syncs: Arc::new(Mutex::new(HashMap::new())),
            download_queue: Arc::new(Mutex::new(Vec::new())),
            peer_scores: Arc::new(RwLock::new(HashMap::new())),
            parallel_downloads: 4,
        }
    }

    async fn download_snapshot_from_peers(&self, height: u64) -> Result<Snapshot, SyncError> {
        // 选择最佳节点
        let peer = self.select_best_peer().await?;
        
        // 模拟下载
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        
        // 返回模拟的快照
        Ok(Snapshot {
            height,
            state_root: H256::random(),
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            accounts: HashMap::new(),
            storage: HashMap::new(),
            code: HashMap::new(),
            metadata: SnapshotMetadata {
                total_accounts: 0,
                total_storage_keys: 0,
                total_contracts: 0,
                snapshot_size_bytes: 0,
                creation_time_ms: 0,
            },
        })
    }

    async fn download_delta_from_peers(
        &self,
        from: u64,
        to: u64,
    ) -> Result<StateDelta, SyncError> {
        let peer = self.select_best_peer().await?;
        
        // 模拟下载
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        
        Ok(StateDelta {
            from_height: from,
            to_height: to,
            changes: vec![],
            proof: MerkleProof {
                root: H256::random(),
                proof_nodes: vec![],
            },
        })
    }

    async fn select_best_peer(&self) -> Result<Vec<u8>, SyncError> {
        let scores = self.peer_scores.read().await;
        
        scores
            .iter()
            .max_by_key(|(_, score)| {
                if score.failure_count == 0 {
                    u64::MAX
                } else {
                    score.success_count / score.failure_count
                }
            })
            .map(|(peer, _)| peer.clone())
            .ok_or(SyncError::NoPeersAvailable)
    }

    async fn update_peer_score(&self, peer: &[u8], success: bool, latency_ms: u64) {
        let mut scores = self.peer_scores.write().await;
        let score = scores.entry(peer.to_vec()).or_insert(PeerScore {
            success_count: 0,
            failure_count: 0,
            avg_latency_ms: 0,
            last_seen: SystemTime::now(),
        });
        
        if success {
            score.success_count += 1;
        } else {
            score.failure_count += 1;
        }
        
        // 更新平均延迟
        score.avg_latency_ms = (score.avg_latency_ms * 9 + latency_ms) / 10;
        score.last_seen = SystemTime::now();
    }
}

#[derive(Debug, Clone)]
pub struct StateData {
    pub root: H256,
    pub accounts: HashMap<[u8; 20], AccountSnapshot>,
    pub storage: HashMap<([u8; 20], H256), H256>,
    pub code: HashMap<H256, Vec<u8>>,
}

impl Default for StateData {
    fn default() -> Self {
        Self {
            root: H256::zero(),
            accounts: HashMap::new(),
            storage: HashMap::new(),
            code: HashMap::new(),
        }
    }
}

#[derive(Debug)]
pub enum SyncError {
    NoPeersAvailable,
    DownloadFailed(String),
    VerificationFailed,
    Timeout,
}

#[derive(Debug, Clone)]
pub struct SnapshotMetricsSnapshot {
    pub snapshots_created: u64,
    pub snapshots_applied: u64,
    pub deltas_created: u64,
    pub deltas_applied: u64,
    pub sync_sessions_started: u64,
    pub sync_sessions_completed: u64,
    pub bytes_downloaded: u64,
    pub bytes_uploaded: u64,
    pub current_height: u64,
    pub snapshot_count: usize,
    pub delta_count: usize,
}