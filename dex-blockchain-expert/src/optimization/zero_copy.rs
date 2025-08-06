use std::sync::Arc;
use std::ops::Deref;
use std::ptr;
use std::slice;
use bytes::{Bytes, BytesMut, BufMut};
use std::io::{self, Read, Write};

// 零拷贝优化实现
// 核心技术：
// 1. 使用Arc共享数据，避免复制
// 2. 使用Bytes实现零拷贝的字节操作
// 3. 内存映射文件(mmap)
// 4. sendfile系统调用

/// 零拷贝缓冲区
#[derive(Clone)]
pub struct ZeroCopyBuffer {
    data: Arc<Bytes>,
    offset: usize,
    len: usize,
}

impl ZeroCopyBuffer {
    pub fn new(data: Vec<u8>) -> Self {
        Self {
            data: Arc::new(Bytes::from(data)),
            offset: 0,
            len: 0,
        }
    }

    pub fn from_bytes(bytes: Bytes) -> Self {
        let len = bytes.len();
        Self {
            data: Arc::new(bytes),
            offset: 0,
            len,
        }
    }

    pub fn slice(&self, start: usize, end: usize) -> Self {
        assert!(start <= end);
        assert!(end <= self.len);
        
        Self {
            data: self.data.clone(),
            offset: self.offset + start,
            len: end - start,
        }
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.data[self.offset..self.offset + self.len]
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn split_at(&self, mid: usize) -> (Self, Self) {
        assert!(mid <= self.len);
        
        let left = Self {
            data: self.data.clone(),
            offset: self.offset,
            len: mid,
        };
        
        let right = Self {
            data: self.data.clone(),
            offset: self.offset + mid,
            len: self.len - mid,
        };
        
        (left, right)
    }

    pub fn concat(&self, other: &Self) -> Self {
        // 如果两个buffer来自同一个Arc且相邻，可以合并
        if Arc::ptr_eq(&self.data, &other.data) &&
           self.offset + self.len == other.offset {
            return Self {
                data: self.data.clone(),
                offset: self.offset,
                len: self.len + other.len,
            };
        }
        
        // 否则需要创建新的buffer
        let mut result = BytesMut::with_capacity(self.len + other.len);
        result.put_slice(self.as_slice());
        result.put_slice(other.as_slice());
        
        Self::from_bytes(result.freeze())
    }
}

/// 零拷贝字符串
pub struct ZeroCopyString {
    buffer: ZeroCopyBuffer,
}

impl ZeroCopyString {
    pub fn new(s: String) -> Self {
        Self {
            buffer: ZeroCopyBuffer::new(s.into_bytes()),
        }
    }

    pub fn from_buffer(buffer: ZeroCopyBuffer) -> Result<Self, std::str::Utf8Error> {
        std::str::from_utf8(buffer.as_slice())?;
        Ok(Self { buffer })
    }

    pub fn as_str(&self) -> &str {
        unsafe {
            // 安全性：在from_buffer中已经验证了UTF-8
            std::str::from_utf8_unchecked(self.buffer.as_slice())
        }
    }

    pub fn substring(&self, start: usize, end: usize) -> Result<Self, std::str::Utf8Error> {
        let s = self.as_str();
        
        // 确保在字符边界切分
        if !s.is_char_boundary(start) || !s.is_char_boundary(end) {
            return Err(std::str::Utf8Error::valid_up_to(start));
        }
        
        Ok(Self {
            buffer: self.buffer.slice(start, end),
        })
    }
}

impl Deref for ZeroCopyString {
    type Target = str;
    
    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

/// 内存映射文件
pub struct MmapFile {
    #[cfg(unix)]
    mmap: memmap2::Mmap,
    #[cfg(not(unix))]
    data: Vec<u8>,
}

impl MmapFile {
    #[cfg(unix)]
    pub fn open(path: &str) -> io::Result<Self> {
        use std::fs::File;
        use memmap2::MmapOptions;
        
        let file = File::open(path)?;
        let mmap = unsafe { MmapOptions::new().map(&file)? };
        
        Ok(Self { mmap })
    }
    
    #[cfg(not(unix))]
    pub fn open(path: &str) -> io::Result<Self> {
        use std::fs;
        let data = fs::read(path)?;
        Ok(Self { data })
    }

    pub fn as_slice(&self) -> &[u8] {
        #[cfg(unix)]
        return &self.mmap[..];
        
        #[cfg(not(unix))]
        return &self.data[..];
    }

    pub fn len(&self) -> usize {
        self.as_slice().len()
    }
}

/// 零拷贝I/O操作
pub struct ZeroCopyIO;

impl ZeroCopyIO {
    /// 使用sendfile进行零拷贝文件传输（仅Linux）
    #[cfg(target_os = "linux")]
    pub fn sendfile(
        out_fd: i32,
        in_fd: i32,
        offset: Option<&mut i64>,
        count: usize,
    ) -> io::Result<usize> {
        use libc::{sendfile, off_t};
        
        let offset_ptr = match offset {
            Some(off) => off as *mut i64 as *mut off_t,
            None => ptr::null_mut(),
        };
        
        let result = unsafe {
            sendfile(out_fd, in_fd, offset_ptr, count)
        };
        
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(result as usize)
        }
    }

    /// splice系统调用（仅Linux）
    #[cfg(target_os = "linux")]
    pub fn splice(
        fd_in: i32,
        off_in: Option<&mut i64>,
        fd_out: i32,
        off_out: Option<&mut i64>,
        len: usize,
        flags: u32,
    ) -> io::Result<usize> {
        use libc::{splice, loff_t};
        
        let off_in_ptr = match off_in {
            Some(off) => off as *mut i64 as *mut loff_t,
            None => ptr::null_mut(),
        };
        
        let off_out_ptr = match off_out {
            Some(off) => off as *mut i64 as *mut loff_t,
            None => ptr::null_mut(),
        };
        
        let result = unsafe {
            splice(fd_in, off_in_ptr, fd_out, off_out_ptr, len, flags)
        };
        
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(result as usize)
        }
    }

    /// 零拷贝写入
    pub fn write_vectored(writer: &mut dyn Write, bufs: &[ZeroCopyBuffer]) -> io::Result<usize> {
        // 构建IoSlice数组
        let slices: Vec<io::IoSlice> = bufs
            .iter()
            .map(|buf| io::IoSlice::new(buf.as_slice()))
            .collect();
        
        writer.write_vectored(&slices)
    }
}

/// 环形缓冲区（用于零拷贝流处理）
pub struct RingBuffer {
    buffer: Vec<u8>,
    capacity: usize,
    read_pos: usize,
    write_pos: usize,
    is_full: bool,
}

impl RingBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            buffer: vec![0; capacity],
            capacity,
            read_pos: 0,
            write_pos: 0,
            is_full: false,
        }
    }

    pub fn write(&mut self, data: &[u8]) -> usize {
        let available = self.available_write();
        let to_write = data.len().min(available);
        
        if to_write == 0 {
            return 0;
        }
        
        let first_part = (self.capacity - self.write_pos).min(to_write);
        let second_part = to_write - first_part;
        
        // 写入第一部分
        self.buffer[self.write_pos..self.write_pos + first_part]
            .copy_from_slice(&data[..first_part]);
        
        // 写入第二部分（如果需要环绕）
        if second_part > 0 {
            self.buffer[..second_part]
                .copy_from_slice(&data[first_part..to_write]);
        }
        
        self.write_pos = (self.write_pos + to_write) % self.capacity;
        
        if to_write > 0 && self.write_pos == self.read_pos {
            self.is_full = true;
        }
        
        to_write
    }

    pub fn read(&mut self, buf: &mut [u8]) -> usize {
        let available = self.available_read();
        let to_read = buf.len().min(available);
        
        if to_read == 0 {
            return 0;
        }
        
        let first_part = (self.capacity - self.read_pos).min(to_read);
        let second_part = to_read - first_part;
        
        // 读取第一部分
        buf[..first_part]
            .copy_from_slice(&self.buffer[self.read_pos..self.read_pos + first_part]);
        
        // 读取第二部分（如果需要环绕）
        if second_part > 0 {
            buf[first_part..to_read]
                .copy_from_slice(&self.buffer[..second_part]);
        }
        
        self.read_pos = (self.read_pos + to_read) % self.capacity;
        self.is_full = false;
        
        to_read
    }

    pub fn read_zero_copy(&self) -> (ZeroCopyBuffer, Option<ZeroCopyBuffer>) {
        let available = self.available_read();
        
        if available == 0 {
            return (ZeroCopyBuffer::from_bytes(Bytes::new()), None);
        }
        
        if self.read_pos < self.write_pos {
            // 连续数据
            let buf = ZeroCopyBuffer::from_bytes(
                Bytes::copy_from_slice(&self.buffer[self.read_pos..self.write_pos])
            );
            (buf, None)
        } else {
            // 分成两部分
            let first = ZeroCopyBuffer::from_bytes(
                Bytes::copy_from_slice(&self.buffer[self.read_pos..self.capacity])
            );
            let second = if self.write_pos > 0 {
                Some(ZeroCopyBuffer::from_bytes(
                    Bytes::copy_from_slice(&self.buffer[..self.write_pos])
                ))
            } else {
                None
            };
            (first, second)
        }
    }

    fn available_write(&self) -> usize {
        if self.is_full {
            0
        } else if self.write_pos >= self.read_pos {
            self.capacity - self.write_pos + self.read_pos
        } else {
            self.read_pos - self.write_pos
        }
    }

    fn available_read(&self) -> usize {
        if self.is_full {
            self.capacity
        } else if self.write_pos >= self.read_pos {
            self.write_pos - self.read_pos
        } else {
            self.capacity - self.read_pos + self.write_pos
        }
    }
}

/// 共享内存区域
pub struct SharedMemoryRegion {
    data: Arc<Vec<u8>>,
    offset: usize,
    len: usize,
}

impl SharedMemoryRegion {
    pub fn new(size: usize) -> Self {
        Self {
            data: Arc::new(vec![0; size]),
            offset: 0,
            len: size,
        }
    }

    pub fn clone_region(&self, offset: usize, len: usize) -> Self {
        assert!(offset + len <= self.len);
        
        Self {
            data: self.data.clone(),
            offset: self.offset + offset,
            len,
        }
    }

    pub unsafe fn as_mut_slice(&mut self) -> &mut [u8] {
        let ptr = Arc::get_mut(&mut self.data)
            .expect("Cannot get mutable reference to shared memory")
            .as_mut_ptr();
        
        slice::from_raw_parts_mut(ptr.add(self.offset), self.len)
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.data[self.offset..self.offset + self.len]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zero_copy_buffer() {
        let data = vec![1, 2, 3, 4, 5];
        let buffer = ZeroCopyBuffer::new(data);
        
        let (left, right) = buffer.split_at(3);
        assert_eq!(left.as_slice(), &[1, 2, 3]);
        assert_eq!(right.as_slice(), &[4, 5]);
        
        let slice = buffer.slice(1, 4);
        assert_eq!(slice.as_slice(), &[2, 3, 4]);
    }

    #[test]
    fn test_ring_buffer() {
        let mut ring = RingBuffer::new(10);
        
        let written = ring.write(b"hello");
        assert_eq!(written, 5);
        
        let mut buf = vec![0; 10];
        let read = ring.read(&mut buf);
        assert_eq!(read, 5);
        assert_eq!(&buf[..5], b"hello");
    }
}