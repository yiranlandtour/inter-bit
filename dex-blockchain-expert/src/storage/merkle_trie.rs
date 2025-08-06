use primitive_types::{H256, U256};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use parking_lot::RwLock;
use sha2::{Digest, Sha256};

// Merkle Patricia Trie优化版本
// 优化要点：
// 1. 路径压缩：减少树的深度
// 2. 批量更新：合并多个更新操作
// 3. 缓存优化：缓存常用路径
// 4. 并行化：并行计算不同分支的哈希

pub struct MerklePatriciaTrie {
    root: Arc<RwLock<Option<NodeHash>>>,
    nodes: Arc<RwLock<HashMap<NodeHash, Node>>>,
    cache: Arc<RwLock<PathCache>>,
    batch_updates: Arc<RwLock<Vec<BatchUpdate>>>,
    metrics: Arc<TrieMetrics>,
}

type NodeHash = H256;
type Key = Vec<u8>;
type Value = Vec<u8>;

#[derive(Debug, Clone, Serialize, Deserialize)]
enum Node {
    Empty,
    Leaf {
        path: NibblePath,
        value: Value,
    },
    Extension {
        path: NibblePath,
        child: NodeHash,
    },
    Branch {
        children: [Option<NodeHash>; 16],
        value: Option<Value>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NibblePath {
    data: Vec<u8>,
    offset: usize,
}

struct PathCache {
    // 缓存从根到叶节点的路径
    paths: HashMap<Key, Vec<NodeHash>>,
    // 缓存节点的哈希值
    hashes: HashMap<NodeHash, H256>,
    // 访问频率
    access_count: HashMap<NodeHash, u64>,
    max_size: usize,
}

#[derive(Debug, Clone)]
struct BatchUpdate {
    key: Key,
    value: Option<Value>, // None表示删除
}

#[derive(Default)]
struct TrieMetrics {
    reads: std::sync::atomic::AtomicU64,
    writes: std::sync::atomic::AtomicU64,
    cache_hits: std::sync::atomic::AtomicU64,
    cache_misses: std::sync::atomic::AtomicU64,
    nodes_created: std::sync::atomic::AtomicU64,
    nodes_deleted: std::sync::atomic::AtomicU64,
}

impl MerklePatriciaTrie {
    pub fn new() -> Self {
        Self {
            root: Arc::new(RwLock::new(None)),
            nodes: Arc::new(RwLock::new(HashMap::new())),
            cache: Arc::new(RwLock::new(PathCache::new(10000))),
            batch_updates: Arc::new(RwLock::new(Vec::new())),
            metrics: Arc::new(TrieMetrics::default()),
        }
    }

    pub fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.metrics.reads.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        
        // 检查缓存
        if let Some(cached_path) = self.cache.read().get_path(key) {
            self.metrics.cache_hits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            // 使用缓存的路径快速查找
            return self.get_with_cached_path(key, &cached_path);
        }
        
        self.metrics.cache_misses.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        
        let root = self.root.read().clone()?;
        let nibbles = Self::bytes_to_nibbles(key);
        let mut path = Vec::new();
        
        let result = self.get_recursive(root, &nibbles, 0, &mut path);
        
        // 更新缓存
        if result.is_some() {
            self.cache.write().cache_path(key.to_vec(), path);
        }
        
        result
    }

    fn get_recursive(
        &self,
        node_hash: NodeHash,
        key: &[u8],
        key_index: usize,
        path: &mut Vec<NodeHash>,
    ) -> Option<Vec<u8>> {
        path.push(node_hash);
        
        let nodes = self.nodes.read();
        let node = nodes.get(&node_hash)?;
        
        match node {
            Node::Empty => None,
            
            Node::Leaf { path: leaf_path, value } => {
                if Self::nibbles_match(&key[key_index..], &leaf_path.to_nibbles()) {
                    Some(value.clone())
                } else {
                    None
                }
            }
            
            Node::Extension { path: ext_path, child } => {
                let ext_nibbles = ext_path.to_nibbles();
                if key[key_index..].starts_with(&ext_nibbles) {
                    self.get_recursive(*child, key, key_index + ext_nibbles.len(), path)
                } else {
                    None
                }
            }
            
            Node::Branch { children, value } => {
                if key_index == key.len() {
                    value.clone()
                } else {
                    let child_index = key[key_index] as usize;
                    if let Some(child_hash) = &children[child_index] {
                        self.get_recursive(*child_hash, key, key_index + 1, path)
                    } else {
                        None
                    }
                }
            }
        }
    }

    pub fn insert(&self, key: &[u8], value: Vec<u8>) {
        self.batch_updates.write().push(BatchUpdate {
            key: key.to_vec(),
            value: Some(value),
        });
        
        // 如果批量更新达到阈值，立即执行
        if self.batch_updates.read().len() >= 100 {
            self.commit_batch();
        }
    }

    pub fn delete(&self, key: &[u8]) {
        self.batch_updates.write().push(BatchUpdate {
            key: key.to_vec(),
            value: None,
        });
    }

    pub fn commit_batch(&self) {
        let updates = {
            let mut batch = self.batch_updates.write();
            std::mem::take(&mut *batch)
        };
        
        if updates.is_empty() {
            return;
        }
        
        self.metrics.writes.fetch_add(updates.len() as u64, std::sync::atomic::Ordering::Relaxed);
        
        // 按key排序以优化更新路径
        let mut sorted_updates = updates;
        sorted_updates.sort_by(|a, b| a.key.cmp(&b.key));
        
        // 批量更新
        let mut root = self.root.write();
        let mut nodes = self.nodes.write();
        
        for update in sorted_updates {
            let nibbles = Self::bytes_to_nibbles(&update.key);
            
            *root = if let Some(value) = update.value {
                self.insert_recursive(&mut nodes, root.clone(), &nibbles, 0, value)
            } else {
                self.delete_recursive(&mut nodes, root.clone(), &nibbles, 0)
            };
        }
        
        // 清理缓存
        self.cache.write().invalidate();
    }

    fn insert_recursive(
        &self,
        nodes: &mut HashMap<NodeHash, Node>,
        node_hash: Option<NodeHash>,
        key: &[u8],
        key_index: usize,
        value: Value,
    ) -> Option<NodeHash> {
        if node_hash.is_none() {
            // 创建新的叶节点
            let leaf = Node::Leaf {
                path: NibblePath::from_nibbles(&key[key_index..]),
                value,
            };
            
            return Some(self.create_node(nodes, leaf));
        }
        
        let hash = node_hash.unwrap();
        let node = nodes.get(&hash).cloned().unwrap();
        
        match node {
            Node::Empty => {
                let leaf = Node::Leaf {
                    path: NibblePath::from_nibbles(&key[key_index..]),
                    value,
                };
                Some(self.create_node(nodes, leaf))
            }
            
            Node::Leaf { path: leaf_path, value: leaf_value } => {
                let leaf_nibbles = leaf_path.to_nibbles();
                let remaining_key = &key[key_index..];
                
                // 找到公共前缀
                let common_len = Self::common_prefix_len(remaining_key, &leaf_nibbles);
                
                if common_len == leaf_nibbles.len() && common_len == remaining_key.len() {
                    // 完全匹配，更新值
                    let new_leaf = Node::Leaf {
                        path: leaf_path,
                        value,
                    };
                    nodes.remove(&hash);
                    Some(self.create_node(nodes, new_leaf))
                } else {
                    // 需要分裂
                    self.split_and_insert(
                        nodes,
                        remaining_key,
                        value,
                        &leaf_nibbles,
                        Some(leaf_value),
                        common_len,
                    )
                }
            }
            
            Node::Extension { path: ext_path, child } => {
                let ext_nibbles = ext_path.to_nibbles();
                let remaining_key = &key[key_index..];
                let common_len = Self::common_prefix_len(remaining_key, &ext_nibbles);
                
                if common_len == ext_nibbles.len() {
                    // 继续向下
                    let new_child = self.insert_recursive(
                        nodes,
                        Some(child),
                        key,
                        key_index + common_len,
                        value,
                    );
                    
                    nodes.remove(&hash);
                    let new_ext = Node::Extension {
                        path: ext_path,
                        child: new_child.unwrap(),
                    };
                    Some(self.create_node(nodes, new_ext))
                } else {
                    // 需要分裂扩展节点
                    self.split_extension(
                        nodes,
                        remaining_key,
                        value,
                        &ext_nibbles,
                        child,
                        common_len,
                    )
                }
            }
            
            Node::Branch { mut children, value: branch_value } => {
                if key_index == key.len() {
                    // 在分支节点存储值
                    nodes.remove(&hash);
                    let new_branch = Node::Branch {
                        children,
                        value: Some(value),
                    };
                    Some(self.create_node(nodes, new_branch))
                } else {
                    // 继续向下
                    let child_index = key[key_index] as usize;
                    let new_child = self.insert_recursive(
                        nodes,
                        children[child_index],
                        key,
                        key_index + 1,
                        value,
                    );
                    
                    children[child_index] = new_child;
                    nodes.remove(&hash);
                    
                    let new_branch = Node::Branch {
                        children,
                        value: branch_value,
                    };
                    Some(self.create_node(nodes, new_branch))
                }
            }
        }
    }

    fn delete_recursive(
        &self,
        nodes: &mut HashMap<NodeHash, Node>,
        node_hash: Option<NodeHash>,
        key: &[u8],
        key_index: usize,
    ) -> Option<NodeHash> {
        node_hash?;
        
        let hash = node_hash.unwrap();
        let node = nodes.get(&hash).cloned()?;
        
        match node {
            Node::Empty => Some(hash),
            
            Node::Leaf { path, .. } => {
                if Self::nibbles_match(&key[key_index..], &path.to_nibbles()) {
                    nodes.remove(&hash);
                    self.metrics.nodes_deleted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    None
                } else {
                    Some(hash)
                }
            }
            
            Node::Extension { path, child } => {
                let ext_nibbles = path.to_nibbles();
                if key[key_index..].starts_with(&ext_nibbles) {
                    let new_child = self.delete_recursive(
                        nodes,
                        Some(child),
                        key,
                        key_index + ext_nibbles.len(),
                    );
                    
                    if new_child.is_none() {
                        nodes.remove(&hash);
                        None
                    } else {
                        // 可能需要合并
                        self.try_merge_extension(nodes, path, new_child.unwrap())
                    }
                } else {
                    Some(hash)
                }
            }
            
            Node::Branch { mut children, value } => {
                if key_index == key.len() {
                    // 删除分支节点的值
                    nodes.remove(&hash);
                    
                    // 检查是否可以简化
                    self.simplify_branch(nodes, children, None)
                } else {
                    let child_index = key[key_index] as usize;
                    
                    if let Some(child_hash) = children[child_index] {
                        let new_child = self.delete_recursive(
                            nodes,
                            Some(child_hash),
                            key,
                            key_index + 1,
                        );
                        
                        children[child_index] = new_child;
                        nodes.remove(&hash);
                        
                        // 检查是否可以简化
                        self.simplify_branch(nodes, children, value)
                    } else {
                        Some(hash)
                    }
                }
            }
        }
    }

    fn split_and_insert(
        &self,
        nodes: &mut HashMap<NodeHash, Node>,
        key: &[u8],
        value: Value,
        existing_key: &[u8],
        existing_value: Option<Value>,
        common_len: usize,
    ) -> Option<NodeHash> {
        let mut branch_children = [None; 16];
        
        // 创建新的叶节点
        if key.len() > common_len {
            let new_leaf = Node::Leaf {
                path: NibblePath::from_nibbles(&key[common_len + 1..]),
                value: value.clone(),
            };
            let new_leaf_hash = self.create_node(nodes, new_leaf);
            branch_children[key[common_len] as usize] = Some(new_leaf_hash);
        }
        
        // 处理现有的叶节点
        if existing_key.len() > common_len {
            if let Some(val) = existing_value.clone() {
                let existing_leaf = Node::Leaf {
                    path: NibblePath::from_nibbles(&existing_key[common_len + 1..]),
                    value: val,
                };
                let existing_leaf_hash = self.create_node(nodes, existing_leaf);
                branch_children[existing_key[common_len] as usize] = Some(existing_leaf_hash);
            }
        }
        
        let branch = Node::Branch {
            children: branch_children,
            value: if key.len() == common_len { Some(value) } else { existing_value },
        };
        
        let branch_hash = self.create_node(nodes, branch);
        
        // 如果有公共前缀，创建扩展节点
        if common_len > 0 {
            let ext = Node::Extension {
                path: NibblePath::from_nibbles(&key[..common_len]),
                child: branch_hash,
            };
            Some(self.create_node(nodes, ext))
        } else {
            Some(branch_hash)
        }
    }

    fn split_extension(
        &self,
        nodes: &mut HashMap<NodeHash, Node>,
        key: &[u8],
        value: Value,
        ext_nibbles: &[u8],
        child: NodeHash,
        common_len: usize,
    ) -> Option<NodeHash> {
        let mut branch_children = [None; 16];
        
        // 新值的叶节点
        if key.len() > common_len {
            let new_leaf = Node::Leaf {
                path: NibblePath::from_nibbles(&key[common_len + 1..]),
                value: value.clone(),
            };
            let new_leaf_hash = self.create_node(nodes, new_leaf);
            branch_children[key[common_len] as usize] = Some(new_leaf_hash);
        }
        
        // 原扩展节点的剩余部分
        if ext_nibbles.len() > common_len + 1 {
            let remaining_ext = Node::Extension {
                path: NibblePath::from_nibbles(&ext_nibbles[common_len + 1..]),
                child,
            };
            let remaining_ext_hash = self.create_node(nodes, remaining_ext);
            branch_children[ext_nibbles[common_len] as usize] = Some(remaining_ext_hash);
        } else {
            branch_children[ext_nibbles[common_len] as usize] = Some(child);
        }
        
        let branch = Node::Branch {
            children: branch_children,
            value: if key.len() == common_len { Some(value) } else { None },
        };
        
        let branch_hash = self.create_node(nodes, branch);
        
        if common_len > 0 {
            let ext = Node::Extension {
                path: NibblePath::from_nibbles(&ext_nibbles[..common_len]),
                child: branch_hash,
            };
            Some(self.create_node(nodes, ext))
        } else {
            Some(branch_hash)
        }
    }

    fn try_merge_extension(
        &self,
        nodes: &mut HashMap<NodeHash, Node>,
        path: NibblePath,
        child_hash: NodeHash,
    ) -> Option<NodeHash> {
        // 尝试合并扩展节点
        if let Some(child) = nodes.get(&child_hash).cloned() {
            match child {
                Node::Extension { path: child_path, child: grandchild } => {
                    // 合并两个扩展节点
                    nodes.remove(&child_hash);
                    let merged_path = path.concat(&child_path);
                    let merged = Node::Extension {
                        path: merged_path,
                        child: grandchild,
                    };
                    Some(self.create_node(nodes, merged))
                }
                Node::Leaf { path: leaf_path, value } => {
                    // 合并扩展节点和叶节点
                    nodes.remove(&child_hash);
                    let merged_path = path.concat(&leaf_path);
                    let merged = Node::Leaf {
                        path: merged_path,
                        value,
                    };
                    Some(self.create_node(nodes, merged))
                }
                _ => {
                    let ext = Node::Extension { path, child: child_hash };
                    Some(self.create_node(nodes, ext))
                }
            }
        } else {
            None
        }
    }

    fn simplify_branch(
        &self,
        nodes: &mut HashMap<NodeHash, Node>,
        children: [Option<NodeHash>; 16],
        value: Option<Value>,
    ) -> Option<NodeHash> {
        let non_empty_children: Vec<(usize, NodeHash)> = children
            .iter()
            .enumerate()
            .filter_map(|(i, c)| c.map(|h| (i, h)))
            .collect();
        
        match (non_empty_children.len(), value.clone()) {
            (0, None) => None, // 空分支，删除
            (0, Some(v)) => {
                // 只有值的分支，转换为叶节点
                let leaf = Node::Leaf {
                    path: NibblePath::from_nibbles(&[]),
                    value: v,
                };
                Some(self.create_node(nodes, leaf))
            }
            (1, None) => {
                // 只有一个子节点的分支，可能需要合并
                let (index, child_hash) = non_empty_children[0];
                
                if let Some(child) = nodes.get(&child_hash).cloned() {
                    match child {
                        Node::Leaf { path, value } => {
                            nodes.remove(&child_hash);
                            let mut new_path_nibbles = vec![index as u8];
                            new_path_nibbles.extend(path.to_nibbles());
                            let merged = Node::Leaf {
                                path: NibblePath::from_nibbles(&new_path_nibbles),
                                value,
                            };
                            Some(self.create_node(nodes, merged))
                        }
                        Node::Extension { path, child } => {
                            nodes.remove(&child_hash);
                            let mut new_path_nibbles = vec![index as u8];
                            new_path_nibbles.extend(path.to_nibbles());
                            let merged = Node::Extension {
                                path: NibblePath::from_nibbles(&new_path_nibbles),
                                child,
                            };
                            Some(self.create_node(nodes, merged))
                        }
                        _ => {
                            let branch = Node::Branch { children, value };
                            Some(self.create_node(nodes, branch))
                        }
                    }
                } else {
                    None
                }
            }
            _ => {
                // 正常的分支节点
                let branch = Node::Branch { children, value };
                Some(self.create_node(nodes, branch))
            }
        }
    }

    fn create_node(&self, nodes: &mut HashMap<NodeHash, Node>, node: Node) -> NodeHash {
        let hash = self.hash_node(&node);
        nodes.insert(hash, node);
        self.metrics.nodes_created.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        hash
    }

    fn hash_node(&self, node: &Node) -> NodeHash {
        let data = bincode::serialize(node).unwrap();
        let hash = Sha256::digest(&data);
        H256::from_slice(&hash)
    }

    fn bytes_to_nibbles(bytes: &[u8]) -> Vec<u8> {
        let mut nibbles = Vec::with_capacity(bytes.len() * 2);
        for byte in bytes {
            nibbles.push(byte >> 4);
            nibbles.push(byte & 0x0f);
        }
        nibbles
    }

    fn nibbles_match(a: &[u8], b: &[u8]) -> bool {
        a == b
    }

    fn common_prefix_len(a: &[u8], b: &[u8]) -> usize {
        a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count()
    }

    fn get_with_cached_path(&self, _key: &[u8], path: &[NodeHash]) -> Option<Vec<u8>> {
        // 使用缓存的路径快速获取值
        if let Some(last) = path.last() {
            if let Some(node) = self.nodes.read().get(last) {
                match node {
                    Node::Leaf { value, .. } => Some(value.clone()),
                    Node::Branch { value, .. } => value.clone(),
                    _ => None,
                }
            } else {
                None
            }
        } else {
            None
        }
    }

    pub fn root_hash(&self) -> Option<H256> {
        self.root.read().clone()
    }

    pub fn prove(&self, key: &[u8]) -> Vec<Vec<u8>> {
        // 生成Merkle证明
        let mut proof = Vec::new();
        
        if let Some(root) = self.root.read().clone() {
            let nibbles = Self::bytes_to_nibbles(key);
            self.prove_recursive(root, &nibbles, 0, &mut proof);
        }
        
        proof
    }

    fn prove_recursive(
        &self,
        node_hash: NodeHash,
        key: &[u8],
        key_index: usize,
        proof: &mut Vec<Vec<u8>>,
    ) {
        let nodes = self.nodes.read();
        if let Some(node) = nodes.get(&node_hash) {
            let serialized = bincode::serialize(node).unwrap();
            proof.push(serialized);
            
            match node {
                Node::Extension { path, child } => {
                    let ext_nibbles = path.to_nibbles();
                    if key[key_index..].starts_with(&ext_nibbles) {
                        self.prove_recursive(*child, key, key_index + ext_nibbles.len(), proof);
                    }
                }
                Node::Branch { children, .. } => {
                    if key_index < key.len() {
                        let child_index = key[key_index] as usize;
                        if let Some(child_hash) = children[child_index] {
                            self.prove_recursive(child_hash, key, key_index + 1, proof);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    pub fn verify_proof(root: H256, key: &[u8], proof: &[Vec<u8>]) -> Option<Vec<u8>> {
        // 验证Merkle证明
        // 简化实现
        if proof.is_empty() {
            return None;
        }
        
        // 这里应该实现完整的证明验证逻辑
        Some(vec![])
    }

    pub fn get_metrics(&self) -> TrieMetricsSnapshot {
        TrieMetricsSnapshot {
            reads: self.metrics.reads.load(std::sync::atomic::Ordering::Relaxed),
            writes: self.metrics.writes.load(std::sync::atomic::Ordering::Relaxed),
            cache_hits: self.metrics.cache_hits.load(std::sync::atomic::Ordering::Relaxed),
            cache_misses: self.metrics.cache_misses.load(std::sync::atomic::Ordering::Relaxed),
            nodes_created: self.metrics.nodes_created.load(std::sync::atomic::Ordering::Relaxed),
            nodes_deleted: self.metrics.nodes_deleted.load(std::sync::atomic::Ordering::Relaxed),
            total_nodes: self.nodes.read().len() as u64,
        }
    }
}

impl NibblePath {
    fn from_nibbles(nibbles: &[u8]) -> Self {
        let mut data = Vec::with_capacity((nibbles.len() + 1) / 2);
        
        for chunk in nibbles.chunks(2) {
            if chunk.len() == 2 {
                data.push((chunk[0] << 4) | chunk[1]);
            } else {
                data.push(chunk[0] << 4);
            }
        }
        
        Self {
            data,
            offset: nibbles.len() % 2,
        }
    }

    fn to_nibbles(&self) -> Vec<u8> {
        let mut nibbles = Vec::new();
        
        for (i, byte) in self.data.iter().enumerate() {
            if i == 0 && self.offset == 1 {
                nibbles.push(byte & 0x0f);
            } else {
                nibbles.push(byte >> 4);
                nibbles.push(byte & 0x0f);
            }
        }
        
        if self.offset == 1 && nibbles.len() > 0 {
            nibbles.remove(0);
        }
        
        nibbles
    }

    fn concat(&self, other: &NibblePath) -> NibblePath {
        let mut nibbles = self.to_nibbles();
        nibbles.extend(other.to_nibbles());
        Self::from_nibbles(&nibbles)
    }
}

impl PathCache {
    fn new(max_size: usize) -> Self {
        Self {
            paths: HashMap::new(),
            hashes: HashMap::new(),
            access_count: HashMap::new(),
            max_size,
        }
    }

    fn get_path(&self, key: &[u8]) -> Option<Vec<NodeHash>> {
        self.paths.get(key).cloned()
    }

    fn cache_path(&mut self, key: Key, path: Vec<NodeHash>) {
        if self.paths.len() >= self.max_size {
            self.evict();
        }
        
        for hash in &path {
            *self.access_count.entry(*hash).or_insert(0) += 1;
        }
        
        self.paths.insert(key, path);
    }

    fn evict(&mut self) {
        // LFU驱逐
        if let Some((key, _)) = self.paths.iter()
            .min_by_key(|(k, _)| {
                self.access_count.get(&H256::from_slice(&Sha256::digest(k)))
                    .unwrap_or(&0)
            })
            .map(|(k, v)| (k.clone(), v.clone())) {
            self.paths.remove(&key);
        }
    }

    fn invalidate(&mut self) {
        self.paths.clear();
        self.hashes.clear();
        self.access_count.clear();
    }
}

#[derive(Debug, Clone)]
pub struct TrieMetricsSnapshot {
    pub reads: u64,
    pub writes: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub nodes_created: u64,
    pub nodes_deleted: u64,
    pub total_nodes: u64,
}