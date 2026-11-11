use anyhow::{bail, Result};
use std::ffi::CString;
use std::os::raw::{c_char, c_int};

#[repr(C)]
struct LuacBuffer {
    data: *mut u8,
    len: usize,
    cap: usize,
}

impl LuacBuffer {
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
            bail!("Lua compiler returned a null buffer with non-zero length");
        }
        // SAFETY: C 在释放前保证 data/len 对应一段有效缓冲区，OwnedBuffer 保持其生命周期。
        Ok(unsafe { std::slice::from_raw_parts(self.data, self.len) })
    }
}

type CompileFn = unsafe extern "C" fn(*const u8, usize, *const c_char, c_int, *mut LuacBuffer, *mut LuacBuffer) -> c_int;
type FreeFn = unsafe extern "C" fn(*mut LuacBuffer);

unsafe extern "C" {
    fn luac32_compile(source: *const u8, source_len: usize, chunk_name: *const c_char, debug_mode: c_int, output: *mut LuacBuffer, error: *mut LuacBuffer) -> c_int;
    fn luac64_compile(source: *const u8, source_len: usize, chunk_name: *const c_char, debug_mode: c_int, output: *mut LuacBuffer, error: *mut LuacBuffer) -> c_int;
    fn luac32_free(buffer: *mut LuacBuffer);
    fn luac64_free(buffer: *mut LuacBuffer);
}

struct OwnedBuffer {
    inner: LuacBuffer,
    free: FreeFn,
}

impl OwnedBuffer {
    fn new(free: FreeFn) -> Self {
        Self { inner: LuacBuffer::null(), free }
    }
}

impl Drop for OwnedBuffer {
    fn drop(&mut self) {
        // SAFETY: inner 由同一 Lua 变体分配，free 是该变体提供的配对释放函数。
        unsafe { (self.free)(&mut self.inner) }
    }
}

pub(crate) fn compile(source: &[u8], chunk_name: &str, debug_mode: u8, bitw: u32) -> Result<Vec<u8>> {
    let (compile_fn, free_fn) = match bitw {
        32 => (luac32_compile as CompileFn, luac32_free as FreeFn),
        64 => (luac64_compile as CompileFn, luac64_free as FreeFn),
        _ => bail!("unsupported Lua bitness: {bitw}"),
    };
    let chunk_name = CString::new(chunk_name).map_err(|_| anyhow::anyhow!("chunk name contains interior NUL"))?;
    let mut output = OwnedBuffer::new(free_fn);
    let mut error = OwnedBuffer::new(free_fn);
    // SAFETY: 输入指针在本次调用期间有效，C 只通过输出指针写入两个独立缓冲区。
    let status = unsafe {
        compile_fn(
            source.as_ptr(),
            source.len(),
            chunk_name.as_ptr(),
            c_int::from(crate::normalize_luac_debug(debug_mode)),
            &mut output.inner,
            &mut error.inner,
        )
    };
    if status != 0 {
        let message = String::from_utf8_lossy(error.inner.as_bytes()?).trim().to_owned();
        if message.is_empty() {
            bail!("Lua {bitw}-bit compiler failed with status {status}");
        }
        bail!(message);
    }
    Ok(output.inner.as_bytes()?.to_vec())
}

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};

    const GOLDEN_SOURCE: &[u8] = br#"local long = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
local big = 0x123456789ABCD
local function outer(a)
    local b = a + 1
    return function(c)
        return b + c + big
    end
end
print(outer(2)(3), long)
"#;

    fn golden_cases() -> [(u32, u8, &'static str); 8] {
        [
            (32, 0, "cd6d832dfd4194474263aeb0296734929a8e36b3a35876c7843646b79caf2615"),
            (32, 1, "30586d4fcab7baba158b9a6230787908e8a624eef0e8a9a9c49359627b40ead9"),
            (32, 2, "c4a7584cbae7b815bb87e838cc1efe1d450f29b3b359054635d418c467ec7c82"),
            (32, 99, "b517755f0cdc6e2b1fddfc1aae9d39ad03b58196e92c5134af17b57e26d459ec"),
            (64, 0, "fd7eb00747c2760ba5718c411363e4343c30c2e4dcd57854f07f26947a619b6e"),
            (64, 1, "8f979416760330f9088ce4beffb558918edd5661110cbce5dfb0a3fc54aee35e"),
            (64, 2, "9a293e31f4cbb8eef307a26d1334c1e5980b81b3a6e75e01a4e7e3a70070be6e"),
            (64, 99, "a5ae258c1bffec36149333f563a4efc82935b89b6a501a64159cdb8469262a2d"),
        ]
    }

    fn sha256(data: &[u8]) -> String {
        format!("{:x}", Sha256::digest(data))
    }

    #[test]
    fn bytecode_matches_embedded_helper() {
        for (bitw, debug_mode, expected) in golden_cases() {
            let bytecode = super::compile(GOLDEN_SOURCE, "@main.lua", debug_mode, bitw).unwrap();
            assert_eq!(sha256(&bytecode), expected, "bitw={bitw}, debug_mode={debug_mode}");
        }
    }

    #[test]
    fn header_sizes_match_requested_bitness() {
        let bytecode32 = super::compile(b"return 1", "@test.lua", 99, 32).unwrap();
        assert_eq!((bytecode32[15], bytecode32[16]), (4, 4));
        let bytecode64 = super::compile(b"return 1", "@test.lua", 99, 64).unwrap();
        assert_eq!((bytecode64[15], bytecode64[16]), (8, 8));
    }

    #[test]
    fn unsupported_bitness_is_rejected() {
        let error = super::compile(b"return 1", "@test.lua", 99, 16).unwrap_err();
        assert!(error.to_string().contains("unsupported Lua bitness: 16"), "{error}");
    }

    #[test]
    fn syntax_error_contains_chunk_name() {
        let error = super::compile(b"if then end end", "@bad.lua", 99, 32).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("bad.lua"), "{message}");
        assert!(message.contains("near"), "{message}");
    }

    #[test]
    fn empty_source_produces_bytecode() {
        let bytecode = super::compile(b"", "@empty.lua", 99, 32).unwrap();
        assert_eq!(&bytecode[..4], b"\x1bLua");
    }

    #[test]
    fn chunk_name_with_nul_is_rejected() {
        let error = super::compile(b"return 1", "@bad\0name.lua", 99, 32).unwrap_err();
        assert!(error.to_string().contains("chunk name"), "{error}");
    }

    #[test]
    fn output_buffer_grows_beyond_initial_capacity() {
        let source = format!("return \"{}\"", "a".repeat(20_000));
        let bytecode = super::compile(source.as_bytes(), "@large.lua", 0, 64).unwrap();
        assert_eq!(&bytecode[..4], b"\x1bLua");
        assert!(bytecode.len() > 20_000);
    }

    #[test]
    fn concurrent_bitness_calls_are_isolated() {
        let threads: Vec<_> = golden_cases()
            .into_iter()
            .map(|(bitw, debug_mode, expected)| {
                std::thread::spawn(move || {
                    let bytecode = super::compile(GOLDEN_SOURCE, "@main.lua", debug_mode, bitw).unwrap();
                    assert_eq!(sha256(&bytecode), expected, "bitw={bitw}, debug_mode={debug_mode}");
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
    }
}
