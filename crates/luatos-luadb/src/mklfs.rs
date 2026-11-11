use anyhow::{bail, ensure, Context, Result};
use std::ffi::CString;
use std::os::raw::{c_char, c_int};
use std::path::Path;

#[repr(C)]
struct MklfsBuffer {
    data: *mut u8,
    len: usize,
    cap: usize,
}

impl MklfsBuffer {
    fn null() -> Self {
        Self {
            data: std::ptr::null_mut(),
            len: 0,
            cap: 0,
        }
    }

    fn as_bytes(&self) -> Result<&[u8]> {
        if self.len == 0 {
            return Ok(&[]);
        }
        if self.data.is_null() {
            bail!("mklfs returned a null error buffer with non-zero length");
        }
        // SAFETY: C 在释放前保证 data/len 对应一段有效缓冲区，OwnedBuffer 保持其生命周期。
        Ok(unsafe { std::slice::from_raw_parts(self.data, self.len) })
    }
}

unsafe extern "C" {
    fn mklfs_build(
        source_dirs: *const *const c_char,
        source_dir_count: usize,
        fs_size: usize,
        block_size: usize,
        output: *mut u8,
        output_len: usize,
        error: *mut MklfsBuffer,
    ) -> c_int;
    fn mklfs_free(buffer: *mut MklfsBuffer);
}

struct OwnedBuffer {
    inner: MklfsBuffer,
}

impl OwnedBuffer {
    fn new() -> Self {
        Self { inner: MklfsBuffer::null() }
    }
}

impl Drop for OwnedBuffer {
    fn drop(&mut self) {
        // SAFETY: inner 由 mklfs_build 初始化，mklfs_free 是配对释放函数。
        unsafe { mklfs_free(&mut self.inner) }
    }
}

pub(crate) fn build(source_dirs: &[&Path], fs_size: usize, block_size: usize) -> Result<Vec<u8>> {
    ensure!(fs_size <= u32::MAX as usize, "mklfs filesystem size exceeds the LittleFS 32-bit limit");
    ensure!(block_size <= u32::MAX as usize, "mklfs block size exceeds the LittleFS 32-bit limit");
    ensure!(
        block_size >= 256 && block_size.is_multiple_of(256),
        "invalid size parameters: block size must be a multiple of 256"
    );

    let source_dirs = source_dirs
        .iter()
        .map(|dir| {
            let path = dir.to_str().context("non-UTF8 path")?;
            CString::new(path).map_err(|_| anyhow::anyhow!("source directory path contains interior NUL"))
        })
        .collect::<Result<Vec<_>>>()?;
    let source_dir_ptrs = source_dirs.iter().map(|dir| dir.as_ptr()).collect::<Vec<_>>();

    let mut image = Vec::new();
    image
        .try_reserve_exact(fs_size)
        .map_err(|error| anyhow::anyhow!("failed to allocate {fs_size} bytes for mklfs image: {error}"))?;
    image.resize(fs_size, 0xFF);
    let mut error = OwnedBuffer::new();
    // SAFETY: 目录字符串、指针数组和输出 Vec 在调用期间均有效；C 只写入 output_len 字节并初始化 error。
    let status = unsafe {
        mklfs_build(
            source_dir_ptrs.as_ptr(),
            source_dir_ptrs.len(),
            fs_size,
            block_size,
            image.as_mut_ptr(),
            image.len(),
            &mut error.inner,
        )
    };
    if status != 0 {
        let message = String::from_utf8_lossy(error.inner.as_bytes()?).trim().to_owned();
        if message.is_empty() {
            bail!("mklfs failed with status {status}");
        }
        bail!("mklfs failed: {message}");
    }
    Ok(image)
}
