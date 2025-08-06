use tokio::io::{AsyncReadExt, AsyncWriteExt, AsyncBufReadExt, BufReader, BufWriter};
use tokio::fs::{File, OpenOptions};
use tokio::sync::{mpsc, oneshot, Semaphore};
use std::sync::Arc;
use std::path::Path;
use std::io;
use futures::stream::{self, StreamExt};
use bytes::{Bytes, BytesMut, BufMut};

// 异步I/O优化
// 核心技术：
// 1. io_uring (Linux 5.1+)
// 2. 异步文件操作
// 3. 并发I/O调度
// 4. 缓冲区管理

/// 异步I/O管理器
pub struct AsyncIOManager {
    io_semaphore: Arc<Semaphore>,
    read_buffer_pool: Arc<super::memory_pool::ObjectPool<BytesMut>>,
    write_queue: Arc<mpsc::UnboundedSender<WriteRequest>>,
    metrics: Arc<IOMetrics>,
}

struct WriteRequest {
    path: String,
    data: Bytes,
    response: oneshot::Sender<io::Result<()>>,
}

#[derive(Default)]
struct IOMetrics {
    reads_completed: std::sync::atomic::AtomicU64,
    writes_completed: std::sync::atomic::AtomicU64,
    bytes_read: std::sync::atomic::AtomicU64,
    bytes_written: std::sync::atomic::AtomicU64,
    avg_read_latency_us: std::sync::atomic::AtomicU64,
    avg_write_latency_us: std::sync::atomic::AtomicU64,
}

impl AsyncIOManager {
    pub fn new(max_concurrent_io: usize) -> Self {
        let (write_tx, mut write_rx) = mpsc::unbounded_channel();
        
        // 启动写入工作线程
        tokio::spawn(async move {
            while let Some(req) = write_rx.recv().await {
                let result = Self::process_write(req.path, req.data).await;
                let _ = req.response.send(result);
            }
        });
        
        Self {
            io_semaphore: Arc::new(Semaphore::new(max_concurrent_io)),
            read_buffer_pool: Arc::new(super::memory_pool::ObjectPool::new(
                || BytesMut::with_capacity(4096),
                |buf| buf.clear(),
                100,
            )),
            write_queue: Arc::new(write_tx),
            metrics: Arc::new(IOMetrics::default()),
        }
    }

    /// 异步读取文件
    pub async fn read_file(&self, path: impl AsRef<Path>) -> io::Result<Bytes> {
        let _permit = self.io_semaphore.acquire().await.unwrap();
        let start = std::time::Instant::now();
        
        let mut file = File::open(path).await?;
        let mut buffer = self.read_buffer_pool.acquire();
        
        let mut total_read = 0;
        loop {
            let n = file.read_buf(&mut *buffer).await?;
            if n == 0 {
                break;
            }
            total_read += n;
        }
        
        let result = buffer.split().freeze();
        
        // 更新指标
        self.metrics.bytes_read.fetch_add(total_read as u64, std::sync::atomic::Ordering::Relaxed);
        self.metrics.reads_completed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        
        let latency = start.elapsed().as_micros() as u64;
        self.update_avg_read_latency(latency);
        
        Ok(result)
    }

    /// 异步写入文件
    pub async fn write_file(&self, path: impl AsRef<Path>, data: Bytes) -> io::Result<()> {
        let (tx, rx) = oneshot::channel();
        
        self.write_queue.send(WriteRequest {
            path: path.as_ref().to_string_lossy().to_string(),
            data: data.clone(),
            response: tx,
        }).map_err(|_| io::Error::new(io::ErrorKind::Other, "Write queue closed"))?;
        
        self.metrics.bytes_written.fetch_add(data.len() as u64, std::sync::atomic::Ordering::Relaxed);
        
        rx.await.map_err(|_| io::Error::new(io::ErrorKind::Other, "Write response failed"))?
    }

    /// 并发读取多个文件
    pub async fn read_files_concurrent(&self, paths: Vec<String>) -> Vec<io::Result<Bytes>> {
        let semaphore = self.io_semaphore.clone();
        
        stream::iter(paths)
            .map(|path| {
                let sem = semaphore.clone();
                async move {
                    let _permit = sem.acquire().await.unwrap();
                    self.read_file(&path).await
                }
            })
            .buffer_unordered(10)
            .collect()
            .await
    }

    /// 流式读取大文件
    pub async fn stream_read(&self, path: impl AsRef<Path>, chunk_size: usize) -> io::Result<FileStream> {
        let file = File::open(path).await?;
        
        Ok(FileStream {
            reader: BufReader::with_capacity(chunk_size, file),
            chunk_size,
            semaphore: self.io_semaphore.clone(),
        })
    }

    async fn process_write(path: String, data: Bytes) -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .await?;
        
        file.write_all(&data).await?;
        file.sync_all().await?;
        
        Ok(())
    }

    fn update_avg_read_latency(&self, latency: u64) {
        let current = self.metrics.avg_read_latency_us.load(std::sync::atomic::Ordering::Relaxed);
        let new_avg = (current * 9 + latency) / 10;
        self.metrics.avg_read_latency_us.store(new_avg, std::sync::atomic::Ordering::Relaxed);
    }
}

/// 文件流读取器
pub struct FileStream {
    reader: BufReader<File>,
    chunk_size: usize,
    semaphore: Arc<Semaphore>,
}

impl FileStream {
    pub async fn next_chunk(&mut self) -> io::Result<Option<Bytes>> {
        let _permit = self.semaphore.acquire().await.unwrap();
        
        let mut buffer = BytesMut::with_capacity(self.chunk_size);
        let n = self.reader.read_buf(&mut buffer).await?;
        
        if n == 0 {
            Ok(None)
        } else {
            Ok(Some(buffer.freeze()))
        }
    }
}

// io_uring support is commented out as it requires additional system dependencies
// To enable io_uring support:
// 1. Add io-uring = "0.6" to Cargo.toml
// 2. Uncomment the implementation below
// 3. Ensure you're running on Linux kernel 5.1+

/*
#[cfg(target_os = "linux")]
pub mod io_uring_support {
    // Full io_uring implementation would go here
    // This provides significant performance improvements for async I/O on Linux
}
*/

/// 直接I/O支持
pub struct DirectIO;

impl DirectIO {
    #[cfg(unix)]
    pub async fn open_direct(path: impl AsRef<Path>) -> io::Result<File> {
        use std::os::unix::fs::OpenOptionsExt;
        
        // O_DIRECT flag for direct I/O (bypasses page cache)
        const O_DIRECT: i32 = 0x4000; // Linux-specific value
        
        OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(O_DIRECT)
            .open(path)
            .await
    }

    #[cfg(not(unix))]
    pub async fn open_direct(path: impl AsRef<Path>) -> io::Result<File> {
        File::open(path).await
    }

    /// 对齐的缓冲区分配
    pub fn aligned_buffer(size: usize, align: usize) -> Vec<u8> {
        let layout = std::alloc::Layout::from_size_align(size, align)
            .expect("Invalid layout");
        
        let ptr = unsafe { std::alloc::alloc(layout) };
        if ptr.is_null() {
            panic!("Failed to allocate aligned buffer");
        }
        
        unsafe {
            Vec::from_raw_parts(ptr, size, size)
        }
    }
}

/// 预读取优化
pub struct Prefetcher {
    cache: Arc<dashmap::DashMap<String, Bytes>>,
    prefetch_queue: Arc<mpsc::UnboundedSender<String>>,
}

impl Prefetcher {
    pub fn new() -> Self {
        let cache = Arc::new(dashmap::DashMap::new());
        let (tx, mut rx) = mpsc::unbounded_channel();
        
        let cache_clone = cache.clone();
        
        // 预读取工作线程
        tokio::spawn(async move {
            while let Some(path) = rx.recv().await {
                if !cache_clone.contains_key(&path) {
                    if let Ok(data) = tokio::fs::read(&path).await {
                        cache_clone.insert(path, Bytes::from(data));
                    }
                }
            }
        });
        
        Self {
            cache,
            prefetch_queue: Arc::new(tx),
        }
    }

    pub fn prefetch(&self, path: impl Into<String>) {
        let _ = self.prefetch_queue.send(path.into());
    }

    pub async fn read(&self, path: impl AsRef<Path>) -> io::Result<Bytes> {
        let path_str = path.as_ref().to_string_lossy().to_string();
        
        if let Some(data) = self.cache.get(&path_str) {
            Ok(data.clone())
        } else {
            let data = tokio::fs::read(path).await?;
            let bytes = Bytes::from(data);
            self.cache.insert(path_str, bytes.clone());
            Ok(bytes)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_async_io_manager() {
        let manager = AsyncIOManager::new(10);
        
        // 写入测试
        let data = Bytes::from("test data");
        manager.write_file("/tmp/test.txt", data.clone()).await.unwrap();
        
        // 读取测试
        let read_data = manager.read_file("/tmp/test.txt").await.unwrap();
        assert_eq!(read_data, data);
    }

    #[tokio::test]
    async fn test_prefetcher() {
        let prefetcher = Prefetcher::new();
        
        // 预读取
        prefetcher.prefetch("/tmp/test.txt");
        
        // 等待预读取完成
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        
        // 应该从缓存读取
        let _ = prefetcher.read("/tmp/test.txt").await;
    }
}