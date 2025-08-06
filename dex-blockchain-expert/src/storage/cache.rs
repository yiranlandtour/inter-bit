use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use parking_lot::RwLock;
use std::time::{Duration, Instant};

pub struct MemoryCache {
    data: Arc<RwLock<HashMap<String, CacheEntry>>>,
    lru_queue: Arc<RwLock<VecDeque<String>>>,
    max_size: usize,
    current_size: Arc<RwLock<usize>>,
    access_count: Arc<RwLock<HashMap<String, usize>>>,
}

struct CacheEntry {
    value: Vec<u8>,
    size: usize,
    last_access: Instant,
    access_count: usize,
}

impl MemoryCache {
    pub fn new(max_size: usize) -> Self {
        Self {
            data: Arc::new(RwLock::new(HashMap::new())),
            lru_queue: Arc::new(RwLock::new(VecDeque::new())),
            max_size,
            current_size: Arc::new(RwLock::new(0)),
            access_count: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn get(&self, key: &str) -> Option<Vec<u8>> {
        let mut data = self.data.write();
        
        if let Some(entry) = data.get_mut(key) {
            entry.last_access = Instant::now();
            entry.access_count += 1;
            
            // 更新LRU队列
            let mut queue = self.lru_queue.write();
            queue.retain(|k| k != key);
            queue.push_back(key.to_string());
            
            // 更新访问计数
            let mut access_count = self.access_count.write();
            *access_count.entry(key.to_string()).or_insert(0) += 1;
            
            Some(entry.value.clone())
        } else {
            None
        }
    }

    pub fn put(&self, key: String, value: Vec<u8>) {
        let size = value.len();
        
        // 如果单个条目太大，直接返回
        if size > self.max_size {
            return;
        }
        
        let mut data = self.data.write();
        let mut current_size = self.current_size.write();
        
        // 如果key已存在，先移除旧值
        if let Some(old_entry) = data.remove(&key) {
            *current_size -= old_entry.size;
        }
        
        // 检查是否需要驱逐
        while *current_size + size > self.max_size {
            self.evict_one(&mut data, &mut current_size);
        }
        
        // 插入新条目
        let entry = CacheEntry {
            value,
            size,
            last_access: Instant::now(),
            access_count: 1,
        };
        
        data.insert(key.clone(), entry);
        *current_size += size;
        
        // 更新LRU队列
        let mut queue = self.lru_queue.write();
        queue.push_back(key);
    }

    pub fn remove(&self, key: &str) {
        let mut data = self.data.write();
        
        if let Some(entry) = data.remove(key) {
            let mut current_size = self.current_size.write();
            *current_size -= entry.size;
            
            let mut queue = self.lru_queue.write();
            queue.retain(|k| k != key);
            
            let mut access_count = self.access_count.write();
            access_count.remove(key);
        }
    }

    fn evict_one(
        &self,
        data: &mut HashMap<String, CacheEntry>,
        current_size: &mut usize,
    ) {
        // 使用LFU + LRU混合策略
        let mut queue = self.lru_queue.write();
        
        if let Some(key) = queue.pop_front() {
            if let Some(entry) = data.remove(&key) {
                *current_size -= entry.size;
                
                let mut access_count = self.access_count.write();
                access_count.remove(&key);
            }
        }
    }

    pub async fn evict_cold_entries(&self) {
        let threshold = Duration::from_secs(300); // 5分钟未访问
        let now = Instant::now();
        
        let mut data = self.data.write();
        let mut current_size = self.current_size.write();
        let mut to_remove = Vec::new();
        
        for (key, entry) in data.iter() {
            if now.duration_since(entry.last_access) > threshold {
                to_remove.push(key.clone());
                *current_size -= entry.size;
            }
        }
        
        for key in to_remove {
            data.remove(&key);
            
            let mut queue = self.lru_queue.write();
            queue.retain(|k| k != &key);
            
            let mut access_count = self.access_count.write();
            access_count.remove(&key);
        }
    }

    pub fn size(&self) -> usize {
        *self.current_size.read()
    }

    pub fn entry_count(&self) -> usize {
        self.data.read().len()
    }

    pub fn clear(&self) {
        self.data.write().clear();
        self.lru_queue.write().clear();
        self.access_count.write().clear();
        *self.current_size.write() = 0;
    }

    pub fn get_hot_keys(&self, top_n: usize) -> Vec<(String, usize)> {
        let access_count = self.access_count.read();
        let mut items: Vec<_> = access_count.iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        
        items.sort_by(|a, b| b.1.cmp(&a.1));
        items.truncate(top_n);
        items
    }

    pub fn prefetch(&self, keys: Vec<String>) -> Vec<Option<Vec<u8>>> {
        keys.into_iter()
            .map(|key| self.get(&key))
            .collect()
    }

    pub fn batch_get(&self, keys: &[String]) -> Vec<Option<Vec<u8>>> {
        let data = self.data.read();
        keys.iter()
            .map(|key| {
                data.get(key).map(|entry| entry.value.clone())
            })
            .collect()
    }

    pub fn batch_put(&self, items: Vec<(String, Vec<u8>)>) {
        for (key, value) in items {
            self.put(key, value);
        }
    }

    pub fn get_stats(&self) -> CacheStats {
        let data = self.data.read();
        let total_entries = data.len();
        let total_size = *self.current_size.read();
        
        let mut hot_entries = 0;
        let mut cold_entries = 0;
        let threshold = Duration::from_secs(60);
        let now = Instant::now();
        
        for entry in data.values() {
            if now.duration_since(entry.last_access) < threshold {
                hot_entries += 1;
            } else {
                cold_entries += 1;
            }
        }
        
        CacheStats {
            total_entries,
            total_size,
            max_size: self.max_size,
            hot_entries,
            cold_entries,
            hit_rate: 0.0, // 需要额外的统计逻辑
        }
    }
}

#[derive(Debug, Clone)]
pub struct CacheStats {
    pub total_entries: usize,
    pub total_size: usize,
    pub max_size: usize,
    pub hot_entries: usize,
    pub cold_entries: usize,
    pub hit_rate: f64,
}