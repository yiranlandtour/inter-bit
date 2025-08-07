use std::sync::Arc;
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::mem;
use std::ptr;
use std::alloc::{alloc, dealloc, Layout};

// 内存池优化
// 核心技术：
// 1. 对象池：复用对象，减少分配
// 2. Slab分配器：固定大小内存块
// 3. Arena分配器：批量分配，统一释放
// 4. 线程本地缓存：减少锁竞争

/// 通用对象池
pub struct ObjectPool<T> {
    pool: Arc<Mutex<VecDeque<T>>>,
    factory: Arc<dyn Fn() -> T + Send + Sync>,
    reset: Arc<dyn Fn(&mut T) + Send + Sync>,
    max_size: usize,
}

impl<T: Send + 'static> ObjectPool<T> {
    pub fn new<F, R>(factory: F, reset: R, max_size: usize) -> Self
    where
        F: Fn() -> T + Send + Sync + 'static,
        R: Fn(&mut T) + Send + Sync + 'static,
    {
        Self {
            pool: Arc::new(Mutex::new(VecDeque::new())),
            factory: Arc::new(factory),
            reset: Arc::new(reset),
            max_size,
        }
    }

    pub fn acquire(&self) -> PooledObject<T> {
        let obj = {
            let mut pool = self.pool.lock();
            pool.pop_front()
        };

        let obj = obj.unwrap_or_else(|| (self.factory)());
        
        PooledObject {
            object: Some(obj),
            pool: self.pool.clone(),
            reset: self.reset.clone(),
            max_size: self.max_size,
        }
    }

    pub fn clear(&self) {
        self.pool.lock().clear();
    }

    pub fn size(&self) -> usize {
        self.pool.lock().len()
    }
}

/// 池化对象的包装器
pub struct PooledObject<T> {
    object: Option<T>,
    pool: Arc<Mutex<VecDeque<T>>>,
    reset: Arc<dyn Fn(&mut T) + Send + Sync>,
    max_size: usize,
}

impl<T> Drop for PooledObject<T> {
    fn drop(&mut self) {
        if let Some(mut obj) = self.object.take() {
            (self.reset)(&mut obj);
            
            let mut pool = self.pool.lock();
            if pool.len() < self.max_size {
                pool.push_back(obj);
            }
        }
    }
}

impl<T> std::ops::Deref for PooledObject<T> {
    type Target = T;
    
    fn deref(&self) -> &Self::Target {
        self.object.as_ref().unwrap()
    }
}

impl<T> std::ops::DerefMut for PooledObject<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.object.as_mut().unwrap()
    }
}

/// Slab分配器
pub struct SlabAllocator {
    slabs: Arc<Mutex<Vec<Slab>>>,
    block_size: usize,
    slab_size: usize,
}

struct Slab {
    memory: *mut u8,
    layout: Layout,
    free_list: Vec<usize>,
    used_count: usize,
}

impl SlabAllocator {
    pub fn new(block_size: usize, blocks_per_slab: usize) -> Self {
        Self {
            slabs: Arc::new(Mutex::new(Vec::new())),
            block_size,
            slab_size: block_size * blocks_per_slab,
        }
    }

    pub fn allocate(&self) -> *mut u8 {
        let mut slabs = self.slabs.lock();
        
        // 查找有空闲块的slab
        for slab in slabs.iter_mut() {
            if let Some(offset) = slab.free_list.pop() {
                slab.used_count += 1;
                return unsafe { slab.memory.add(offset * self.block_size) };
            }
        }
        
        // 创建新的slab
        let layout = Layout::from_size_align(self.slab_size, mem::align_of::<u8>())
            .expect("Invalid layout");
        
        let memory = unsafe { alloc(layout) };
        if memory.is_null() {
            panic!("Failed to allocate slab");
        }
        
        let blocks_count = self.slab_size / self.block_size;
        let mut free_list = Vec::with_capacity(blocks_count);
        
        // 初始化空闲列表（跳过第一个块，因为我们要返回它）
        for i in (1..blocks_count).rev() {
            free_list.push(i);
        }
        
        slabs.push(Slab {
            memory,
            layout,
            free_list,
            used_count: 1,
        });
        
        memory
    }

    pub fn deallocate(&self, ptr: *mut u8) {
        let mut slabs = self.slabs.lock();
        
        for slab in slabs.iter_mut() {
            let slab_start = slab.memory as usize;
            let slab_end = slab_start + self.slab_size;
            let ptr_addr = ptr as usize;
            
            if ptr_addr >= slab_start && ptr_addr < slab_end {
                let offset = (ptr_addr - slab_start) / self.block_size;
                slab.free_list.push(offset);
                slab.used_count -= 1;
                return;
            }
        }
        
        panic!("Invalid pointer for deallocation");
    }

    pub fn stats(&self) -> SlabStats {
        let slabs = self.slabs.lock();
        
        let total_slabs = slabs.len();
        let total_blocks = total_slabs * (self.slab_size / self.block_size);
        let used_blocks: usize = slabs.iter().map(|s| s.used_count).sum();
        
        SlabStats {
            total_slabs,
            total_blocks,
            used_blocks,
            free_blocks: total_blocks - used_blocks,
            block_size: self.block_size,
        }
    }
}

impl Drop for Slab {
    fn drop(&mut self) {
        unsafe {
            dealloc(self.memory, self.layout);
        }
    }
}

#[derive(Debug, Clone)]
pub struct SlabStats {
    pub total_slabs: usize,
    pub total_blocks: usize,
    pub used_blocks: usize,
    pub free_blocks: usize,
    pub block_size: usize,
}

/// Arena分配器
pub struct Arena {
    chunks: Arc<Mutex<Vec<ArenaChunk>>>,
    current_chunk: Arc<Mutex<Option<ArenaChunk>>>,
    chunk_size: usize,
}

struct ArenaChunk {
    memory: Vec<u8>,
    used: usize,
}

impl Arena {
    pub fn new(chunk_size: usize) -> Self {
        Self {
            chunks: Arc::new(Mutex::new(Vec::new())),
            current_chunk: Arc::new(Mutex::new(Some(ArenaChunk {
                memory: Vec::with_capacity(chunk_size),
                used: 0,
            }))),
            chunk_size,
        }
    }

    pub fn allocate(&self, size: usize, align: usize) -> *mut u8 {
        let mut current = self.current_chunk.lock();
        
        if let Some(ref mut chunk) = *current {
            // 计算对齐后的偏移
            let offset = (chunk.used + align - 1) & !(align - 1);
            
            if offset + size <= chunk.memory.capacity() {
                // 确保有足够的空间
                if chunk.memory.len() < offset + size {
                    chunk.memory.resize(offset + size, 0);
                }
                
                let ptr = unsafe { chunk.memory.as_mut_ptr().add(offset) };
                chunk.used = offset + size;
                return ptr;
            }
        }
        
        // 需要新的chunk
        let mut new_chunk = ArenaChunk {
            memory: vec![0; size.max(self.chunk_size)],
            used: size,
        };
        
        let ptr = new_chunk.memory.as_mut_ptr();
        
        // 保存旧chunk
        if let Some(old_chunk) = current.take() {
            self.chunks.lock().push(old_chunk);
        }
        
        *current = Some(new_chunk);
        
        ptr
    }

    pub fn allocate_copy<T>(&self, value: &T) -> *mut T {
        let size = mem::size_of::<T>();
        let align = mem::align_of::<T>();
        let ptr = self.allocate(size, align) as *mut T;
        
        unsafe {
            ptr::copy_nonoverlapping(value, ptr, 1);
        }
        
        ptr
    }

    pub fn clear(&self) {
        self.chunks.lock().clear();
        *self.current_chunk.lock() = Some(ArenaChunk {
            memory: Vec::with_capacity(self.chunk_size),
            used: 0,
        });
    }

    pub fn stats(&self) -> ArenaStats {
        let chunks = self.chunks.lock();
        let current = self.current_chunk.lock();
        
        let mut total_allocated = 0;
        let mut total_used = 0;
        
        for chunk in chunks.iter() {
            total_allocated += chunk.memory.capacity();
            total_used += chunk.used;
        }
        
        if let Some(ref chunk) = *current {
            total_allocated += chunk.memory.capacity();
            total_used += chunk.used;
        }
        
        ArenaStats {
            chunks_count: chunks.len() + if current.is_some() { 1 } else { 0 },
            total_allocated,
            total_used,
            utilization: if total_allocated > 0 {
                (total_used as f64 / total_allocated as f64) * 100.0
            } else {
                0.0
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct ArenaStats {
    pub chunks_count: usize,
    pub total_allocated: usize,
    pub total_used: usize,
    pub utilization: f64,
}

/// 线程本地内存池
thread_local! {
    static THREAD_LOCAL_POOL: std::cell::RefCell<ThreadLocalPool> = 
        std::cell::RefCell::new(ThreadLocalPool::new());
}

struct ThreadLocalPool {
    small_objects: Vec<Vec<u8>>,
    medium_objects: Vec<Vec<u8>>,
    large_objects: Vec<Vec<u8>>,
}

impl ThreadLocalPool {
    fn new() -> Self {
        Self {
            small_objects: Vec::new(),
            medium_objects: Vec::new(),
            large_objects: Vec::new(),
        }
    }

    fn allocate(&mut self, size: usize) -> Vec<u8> {
        let pool = if size <= 64 {
            &mut self.small_objects
        } else if size <= 1024 {
            &mut self.medium_objects
        } else {
            &mut self.large_objects
        };
        
        pool.pop().unwrap_or_else(|| Vec::with_capacity(size))
    }

    fn deallocate(&mut self, mut vec: Vec<u8>) {
        vec.clear();
        
        let pool = if vec.capacity() <= 64 {
            &mut self.small_objects
        } else if vec.capacity() <= 1024 {
            &mut self.medium_objects
        } else {
            &mut self.large_objects
        };
        
        if pool.len() < 100 {
            pool.push(vec);
        }
    }
}

pub fn thread_local_allocate(size: usize) -> Vec<u8> {
    THREAD_LOCAL_POOL.with(|pool| {
        let mut pool_ref = pool.borrow_mut();
        pool_ref.allocate(size)
    })
}

pub fn thread_local_deallocate(vec: Vec<u8>) {
    THREAD_LOCAL_POOL.with(|pool| {
        let mut pool_ref = pool.borrow_mut();
        pool_ref.deallocate(vec);
    })
}

/// 固定大小的内存池
pub struct FixedSizePool<const SIZE: usize> {
    pool: Arc<Mutex<Vec<Box<[u8; SIZE]>>>>,
    max_size: usize,
}

impl<const SIZE: usize> FixedSizePool<SIZE> {
    pub fn new(max_size: usize) -> Self {
        Self {
            pool: Arc::new(Mutex::new(Vec::with_capacity(max_size))),
            max_size,
        }
    }

    pub fn acquire(&self) -> FixedSizeBuffer<SIZE> {
        let buffer = self.pool.lock().pop()
            .unwrap_or_else(|| Box::new([0u8; SIZE]));
        
        FixedSizeBuffer {
            buffer: Some(buffer),
            pool: self.pool.clone(),
            max_size: self.max_size,
        }
    }

    pub fn size(&self) -> usize {
        self.pool.lock().len()
    }
}

pub struct FixedSizeBuffer<const SIZE: usize> {
    buffer: Option<Box<[u8; SIZE]>>,
    pool: Arc<Mutex<Vec<Box<[u8; SIZE]>>>>,
    max_size: usize,
}

impl<const SIZE: usize> Drop for FixedSizeBuffer<SIZE> {
    fn drop(&mut self) {
        if let Some(buffer) = self.buffer.take() {
            let mut pool = self.pool.lock();
            if pool.len() < self.max_size {
                pool.push(buffer);
            }
        }
    }
}

impl<const SIZE: usize> std::ops::Deref for FixedSizeBuffer<SIZE> {
    type Target = [u8; SIZE];
    
    fn deref(&self) -> &Self::Target {
        self.buffer.as_ref().unwrap()
    }
}

impl<const SIZE: usize> std::ops::DerefMut for FixedSizeBuffer<SIZE> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.buffer.as_mut().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_object_pool() {
        let pool = ObjectPool::new(
            || vec![0u8; 1024],
            |v| v.clear(),
            10
        );
        
        let mut obj1 = pool.acquire();
        obj1.push(1);
        obj1.push(2);
        
        drop(obj1);
        
        let obj2 = pool.acquire();
        assert_eq!(obj2.len(), 0); // 应该被重置了
    }

    #[test]
    fn test_slab_allocator() {
        let slab = SlabAllocator::new(64, 10);
        
        let ptr1 = slab.allocate();
        let ptr2 = slab.allocate();
        
        assert_ne!(ptr1, ptr2);
        
        slab.deallocate(ptr1);
        
        let ptr3 = slab.allocate();
        assert_eq!(ptr1, ptr3); // 应该复用了ptr1
    }

    #[test]
    fn test_arena() {
        let arena = Arena::new(1024);
        
        let ptr1 = arena.allocate(100, 8);
        let ptr2 = arena.allocate(200, 8);
        
        assert_ne!(ptr1, ptr2);
        
        let stats = arena.stats();
        assert!(stats.total_used >= 300);
    }
}