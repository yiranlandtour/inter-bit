use std::sync::Arc;
use tokio::sync::{mpsc, RwLock, Semaphore};
use std::collections::VecDeque;
use std::time::{Duration, Instant};
use futures::future::join_all;

// 批处理优化
// 核心技术：
// 1. 批量聚合：将多个小操作合并
// 2. 延迟执行：等待更多操作以提高批处理效率
// 3. 自适应批大小：根据负载动态调整
// 4. 并行批处理：多个批次并行执行

/// 批处理器
pub struct BatchProcessor<T, R> {
    batch_queue: Arc<RwLock<VecDeque<BatchItem<T, R>>>>,
    processor: Arc<dyn Fn(Vec<T>) -> Vec<R> + Send + Sync>,
    config: BatchConfig,
    metrics: Arc<BatchMetrics>,
    semaphore: Arc<Semaphore>,
}

struct BatchItem<T, R> {
    item: T,
    response: tokio::sync::oneshot::Sender<R>,
    timestamp: Instant,
}

#[derive(Clone)]
pub struct BatchConfig {
    pub max_batch_size: usize,
    pub max_wait_time: Duration,
    pub min_batch_size: usize,
    pub parallel_batches: usize,
    pub adaptive: bool,
}

impl Default for BatchConfig {
    fn default() -> Self {
        Self {
            max_batch_size: 100,
            max_wait_time: Duration::from_millis(10),
            min_batch_size: 10,
            parallel_batches: 4,
            adaptive: true,
        }
    }
}

#[derive(Default)]
struct BatchMetrics {
    total_batches: std::sync::atomic::AtomicU64,
    total_items: std::sync::atomic::AtomicU64,
    avg_batch_size: std::sync::atomic::AtomicU64,
    avg_wait_time_ms: std::sync::atomic::AtomicU64,
}

impl<T, R> BatchProcessor<T, R>
where
    T: Send + 'static,
    R: Send + Clone + 'static,
{
    pub fn new<F>(processor: F, config: BatchConfig) -> Self
    where
        F: Fn(Vec<T>) -> Vec<R> + Send + Sync + 'static,
    {
        let processor = Arc::new(processor);
        let batch_queue = Arc::new(RwLock::new(VecDeque::new()));
        let metrics = Arc::new(BatchMetrics::default());
        let semaphore = Arc::new(Semaphore::new(config.parallel_batches));
        
        let mut processor_instance = Self {
            batch_queue: batch_queue.clone(),
            processor: processor.clone(),
            config: config.clone(),
            metrics: metrics.clone(),
            semaphore: semaphore.clone(),
        };
        
        // 启动批处理循环
        let queue = batch_queue.clone();
        let proc = processor.clone();
        let cfg = config.clone();
        let mtr = metrics.clone();
        let sem = semaphore.clone();
        
        tokio::spawn(async move {
            Self::processing_loop(queue, proc, cfg, mtr, sem).await;
        });
        
        processor_instance
    }

    pub async fn submit(&self, item: T) -> R {
        let (tx, rx) = tokio::sync::oneshot::channel();
        
        self.batch_queue.write().await.push_back(BatchItem {
            item,
            response: tx,
            timestamp: Instant::now(),
        });
        
        rx.await.expect("Batch processor failed")
    }

    pub async fn submit_batch(&self, items: Vec<T>) -> Vec<R> {
        let futures: Vec<_> = items.into_iter()
            .map(|item| self.submit(item))
            .collect();
        
        join_all(futures).await
    }

    async fn processing_loop(
        queue: Arc<RwLock<VecDeque<BatchItem<T, R>>>>,
        processor: Arc<dyn Fn(Vec<T>) -> Vec<R> + Send + Sync>,
        mut config: BatchConfig,
        metrics: Arc<BatchMetrics>,
        semaphore: Arc<Semaphore>,
    ) {
        let mut last_process = Instant::now();
        
        loop {
            tokio::time::sleep(Duration::from_millis(1)).await;
            
            let should_process = {
                let queue = queue.read().await;
                let queue_size = queue.len();
                
                if queue_size == 0 {
                    false
                } else if queue_size >= config.max_batch_size {
                    true
                } else if last_process.elapsed() >= config.max_wait_time && queue_size >= config.min_batch_size {
                    true
                } else if let Some(oldest) = queue.front() {
                    oldest.timestamp.elapsed() >= config.max_wait_time
                } else {
                    false
                }
            };
            
            if should_process {
                // 获取批次
                let batch = {
                    let mut queue = queue.write().await;
                    let batch_size = queue.len().min(config.max_batch_size);
                    
                    let mut batch = Vec::with_capacity(batch_size);
                    for _ in 0..batch_size {
                        if let Some(item) = queue.pop_front() {
                            batch.push(item);
                        }
                    }
                    batch
                };
                
                if !batch.is_empty() {
                    let batch_size = batch.len();
                    let wait_time = batch.iter()
                        .map(|item| item.timestamp.elapsed().as_millis() as u64)
                        .sum::<u64>() / batch_size as u64;
                    
                    // 获取许可并处理
                    let permit = semaphore.acquire().await.unwrap();
                    let processor = processor.clone();
                    let metrics = metrics.clone();
                    
                    tokio::spawn(async move {
                        let items: Vec<T> = batch.into_iter()
                            .map(|b| b.item)
                            .collect();
                        
                        let responses: Vec<tokio::sync::oneshot::Sender<R>> = batch.into_iter()
                            .map(|b| b.response)
                            .collect();
                        
                        let results = processor(items);
                        
                        for (response, result) in responses.into_iter().zip(results.into_iter()) {
                            let _ = response.send(result);
                        }
                        
                        // 更新指标
                        metrics.total_batches.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        metrics.total_items.fetch_add(batch_size as u64, std::sync::atomic::Ordering::Relaxed);
                        
                        let current_avg = metrics.avg_batch_size.load(std::sync::atomic::Ordering::Relaxed);
                        let new_avg = (current_avg * 9 + batch_size as u64) / 10;
                        metrics.avg_batch_size.store(new_avg, std::sync::atomic::Ordering::Relaxed);
                        
                        let current_wait = metrics.avg_wait_time_ms.load(std::sync::atomic::Ordering::Relaxed);
                        let new_wait = (current_wait * 9 + wait_time) / 10;
                        metrics.avg_wait_time_ms.store(new_wait, std::sync::atomic::Ordering::Relaxed);
                        
                        drop(permit);
                    });
                    
                    // 自适应调整
                    if config.adaptive {
                        Self::adapt_config(&mut config, &metrics);
                    }
                    
                    last_process = Instant::now();
                }
            }
        }
    }

    fn adapt_config(config: &mut BatchConfig, metrics: &BatchMetrics) {
        let avg_batch_size = metrics.avg_batch_size.load(std::sync::atomic::Ordering::Relaxed);
        let avg_wait_time = metrics.avg_wait_time_ms.load(std::sync::atomic::Ordering::Relaxed);
        
        // 如果平均批大小接近最大值，增加批大小
        if avg_batch_size as usize > config.max_batch_size * 9 / 10 {
            config.max_batch_size = (config.max_batch_size * 3 / 2).min(1000);
        }
        
        // 如果等待时间太长，减少等待时间
        if avg_wait_time > config.max_wait_time.as_millis() as u64 * 2 {
            config.max_wait_time = Duration::from_millis(
                (config.max_wait_time.as_millis() as u64 * 3 / 4).max(1)
            );
        }
    }

    pub async fn get_metrics(&self) -> BatchMetricsSnapshot {
        BatchMetricsSnapshot {
            total_batches: self.metrics.total_batches.load(std::sync::atomic::Ordering::Relaxed),
            total_items: self.metrics.total_items.load(std::sync::atomic::Ordering::Relaxed),
            avg_batch_size: self.metrics.avg_batch_size.load(std::sync::atomic::Ordering::Relaxed),
            avg_wait_time_ms: self.metrics.avg_wait_time_ms.load(std::sync::atomic::Ordering::Relaxed),
            queue_size: self.batch_queue.read().await.len(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct BatchMetricsSnapshot {
    pub total_batches: u64,
    pub total_items: u64,
    pub avg_batch_size: u64,
    pub avg_wait_time_ms: u64,
    pub queue_size: usize,
}

/// 批量数据库操作
pub struct BatchDatabase {
    write_processor: Arc<BatchProcessor<WriteOp, Result<(), String>>>,
    read_processor: Arc<BatchProcessor<ReadOp, Result<Vec<u8>, String>>>,
}

#[derive(Clone)]
enum WriteOp {
    Put { key: String, value: Vec<u8> },
    Delete { key: String },
}

#[derive(Clone)]
struct ReadOp {
    key: String,
}

impl BatchDatabase {
    pub fn new() -> Self {
        let write_processor = Arc::new(BatchProcessor::new(
            |ops| {
                // 模拟批量写入
                ops.into_iter()
                    .map(|_| Ok(()))
                    .collect()
            },
            BatchConfig::default(),
        ));
        
        let read_processor = Arc::new(BatchProcessor::new(
            |ops| {
                // 模拟批量读取
                ops.into_iter()
                    .map(|_| Ok(vec![0u8; 100]))
                    .collect()
            },
            BatchConfig::default(),
        ));
        
        Self {
            write_processor,
            read_processor,
        }
    }

    pub async fn put(&self, key: String, value: Vec<u8>) -> Result<(), String> {
        self.write_processor.submit(WriteOp::Put { key, value }).await
    }

    pub async fn delete(&self, key: String) -> Result<(), String> {
        self.write_processor.submit(WriteOp::Delete { key }).await
    }

    pub async fn get(&self, key: String) -> Result<Vec<u8>, String> {
        self.read_processor.submit(ReadOp { key }).await
    }

    pub async fn batch_put(&self, items: Vec<(String, Vec<u8>)>) -> Vec<Result<(), String>> {
        let ops: Vec<WriteOp> = items.into_iter()
            .map(|(key, value)| WriteOp::Put { key, value })
            .collect();
        
        self.write_processor.submit_batch(ops).await
    }
}

/// 批量网络请求处理
pub struct BatchNetworkProcessor {
    processor: Arc<BatchProcessor<NetworkRequest, NetworkResponse>>,
}

#[derive(Clone)]
struct NetworkRequest {
    url: String,
    method: String,
    body: Vec<u8>,
}

#[derive(Clone)]
struct NetworkResponse {
    status: u16,
    body: Vec<u8>,
}

impl BatchNetworkProcessor {
    pub fn new(max_concurrent: usize) -> Self {
        let config = BatchConfig {
            max_batch_size: 50,
            max_wait_time: Duration::from_millis(100),
            min_batch_size: 5,
            parallel_batches: max_concurrent,
            adaptive: true,
        };
        
        let processor = Arc::new(BatchProcessor::new(
            |requests| {
                // 模拟批量网络请求
                requests.into_iter()
                    .map(|_| NetworkResponse {
                        status: 200,
                        body: vec![],
                    })
                    .collect()
            },
            config,
        ));
        
        Self { processor }
    }

    pub async fn send_request(&self, url: String, method: String, body: Vec<u8>) -> NetworkResponse {
        self.processor.submit(NetworkRequest { url, method, body }).await
    }
}

/// 批量事务处理
pub struct BatchTransactionProcessor<T> {
    processor: Arc<BatchProcessor<T, TransactionResult>>,
}

#[derive(Clone)]
pub struct TransactionResult {
    pub success: bool,
    pub gas_used: u64,
    pub output: Vec<u8>,
}

impl<T: Send + Clone + 'static> BatchTransactionProcessor<T> {
    pub fn new<F>(executor: F) -> Self
    where
        F: Fn(Vec<T>) -> Vec<TransactionResult> + Send + Sync + 'static,
    {
        let config = BatchConfig {
            max_batch_size: 1000,
            max_wait_time: Duration::from_millis(5),
            min_batch_size: 100,
            parallel_batches: 8,
            adaptive: true,
        };
        
        let processor = Arc::new(BatchProcessor::new(executor, config));
        
        Self { processor }
    }

    pub async fn execute(&self, transaction: T) -> TransactionResult {
        self.processor.submit(transaction).await
    }

    pub async fn execute_batch(&self, transactions: Vec<T>) -> Vec<TransactionResult> {
        self.processor.submit_batch(transactions).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_batch_processor() {
        let processor = BatchProcessor::new(
            |items: Vec<i32>| {
                items.into_iter().map(|i| i * 2).collect()
            },
            BatchConfig::default(),
        );
        
        let results = join_all(vec![
            processor.submit(1),
            processor.submit(2),
            processor.submit(3),
        ]).await;
        
        assert_eq!(results, vec![2, 4, 6]);
    }

    #[tokio::test]
    async fn test_batch_database() {
        let db = BatchDatabase::new();
        
        // 批量写入
        let results = db.batch_put(vec![
            ("key1".to_string(), vec![1, 2, 3]),
            ("key2".to_string(), vec![4, 5, 6]),
        ]).await;
        
        assert!(results.iter().all(|r| r.is_ok()));
        
        // 读取
        let value = db.get("key1".to_string()).await.unwrap();
        assert_eq!(value.len(), 100); // 模拟返回
    }
}