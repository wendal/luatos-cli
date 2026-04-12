//! Embedded Lua 5.3 compiler and mklfs helpers.
//!
//! At build time, `build.rs` compiles helper executables from the bundled
//! C source and embeds them via `include_bytes!()`.  At runtime the
//! appropriate helper is extracted to a per-user cache directory and invoked as
//! a subprocess.

use sha2::{Digest, Sha256};
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, Once, OnceLock};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const HELPER_32_BYTES: &[u8] = include_bytes!(env!("LUA53_HELPER_EMBED_32"));
const HELPER_64_BYTES: &[u8] = include_bytes!(env!("LUA53_HELPER_EMBED_64"));
const MKLFS_BYTES: &[u8] = include_bytes!(env!("MKLFS_HELPER_EMBED"));
const CACHE_SUBDIR: &str = "luatos-cli/lua-helpers";
static CLEANUP_ONCE: Once = Once::new();
static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

// Per-kind mutexes prevent ETXTBSY (text file busy) on Linux when multiple
// threads try to extract and execute the same helper simultaneously.
static LUAC32_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static LUAC64_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static MKLFS_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// Identifies which embedded helper to use.
#[derive(Clone, Copy)]
enum HelperKind {
    Luac32,
    Luac64,
    Mklfs,
}

#[derive(Clone, Copy)]
struct EmbeddedHelper {
    kind: HelperKind,
    payload: &'static [u8],
}

#[derive(serde::Serialize, serde::Deserialize)]
struct HelperMetadata {
    app_version: String,
    helper_name: String,
    sha256: String,
    // legacy field kept for backward compat with existing cache
    #[serde(default)]
    helper_bitness: u32,
}

impl EmbeddedHelper {
    fn name(&self) -> &'static str {
        match self.kind {
            HelperKind::Luac32 => "luac32",
            HelperKind::Luac64 => "luac64",
            HelperKind::Mklfs => "mklfs",
        }
    }

    fn file_prefix(&self) -> &'static str {
        match self.kind {
            HelperKind::Luac32 => "luac32_helper",
            HelperKind::Luac64 => "luac64_helper",
            HelperKind::Mklfs => "mklfs_helper",
        }
    }
}

/// Ensure the embedded Lua helper for the given bit-width is available on disk.
/// Returns the path to the cached executable.
pub fn ensure_embedded_helper(bitw: u32) -> Result<PathBuf, String> {
    let kind = match bitw {
        32 => HelperKind::Luac32,
        64 => HelperKind::Luac64,
        _ => return Err(format!("unsupported Lua bitness: {}", bitw)),
    };
    ensure_helper(kind)
}

/// Ensure the embedded mklfs helper is available on disk.
/// Returns the path to the cached executable.
pub fn ensure_mklfs_helper() -> Result<PathBuf, String> {
    ensure_helper(HelperKind::Mklfs)
}

fn ensure_helper(kind: HelperKind) -> Result<PathBuf, String> {
    // Serialize extraction per helper kind to avoid ETXTBSY on Linux when
    // parallel threads try to write/execute the same binary simultaneously.
    let lock = match kind {
        HelperKind::Luac32 => LUAC32_LOCK.get_or_init(|| Mutex::new(())),
        HelperKind::Luac64 => LUAC64_LOCK.get_or_init(|| Mutex::new(())),
        HelperKind::Mklfs => MKLFS_LOCK.get_or_init(|| Mutex::new(())),
    };
    let _guard = lock.lock().map_err(|e| format!("helper lock poisoned: {e}"))?;

    let cache_dir = helper_cache_dir()?;
    ensure_private_cache_dir(&cache_dir).map_err(|e| format!("failed to prepare helper cache at {}: {}", cache_dir.display(), e))?;
    run_cleanup_once(&cache_dir);

    let helper = helper_for_kind(kind);
    let target = helper_target_path(&cache_dir, helper);
    let metadata_path = metadata_path_for(&target);
    let expected_sha = helper_sha256(helper);

    if helper_file_is_current(&target, &metadata_path, helper, &expected_sha)? {
        return Ok(target);
    }

    remove_if_exists(&target);
    remove_if_exists(&metadata_path);
    write_helper_files(&cache_dir, &target, &metadata_path, helper, &expected_sha)?;
    Ok(target)
}

/// Pre-initialize the helper cache directory (optional, for eager setup).
pub fn init_helper_cache() -> Result<(), String> {
    let cache_dir = helper_cache_dir()?;
    ensure_private_cache_dir(&cache_dir).map_err(|e| format!("failed to prepare helper cache at {}: {}", cache_dir.display(), e))?;
    run_cleanup_once(&cache_dir);
    Ok(())
}

fn helper_for_kind(kind: HelperKind) -> EmbeddedHelper {
    match kind {
        HelperKind::Luac32 => EmbeddedHelper { kind, payload: HELPER_32_BYTES },
        HelperKind::Luac64 => EmbeddedHelper { kind, payload: HELPER_64_BYTES },
        HelperKind::Mklfs => EmbeddedHelper { kind, payload: MKLFS_BYTES },
    }
}

fn helper_cache_dir() -> Result<PathBuf, String> {
    #[cfg(target_os = "windows")]
    {
        if let Some(dir) = env::var_os("LOCALAPPDATA") {
            return Ok(PathBuf::from(dir).join(CACHE_SUBDIR));
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        if let Some(dir) = env::var_os("XDG_CACHE_HOME") {
            return Ok(PathBuf::from(dir).join(CACHE_SUBDIR));
        }
        if let Some(home) = env::var_os("HOME") {
            return Ok(PathBuf::from(home).join(".cache").join(CACHE_SUBDIR));
        }
    }

    let temp = env::temp_dir();
    if temp.as_os_str().is_empty() {
        Err("unable to determine a helper cache directory".to_string())
    } else {
        Ok(temp.join(CACHE_SUBDIR))
    }
}

fn ensure_private_cache_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn helper_target_path(cache_dir: &Path, helper: EmbeddedHelper) -> PathBuf {
    cache_dir.join(format!("{}-{}-{}{}", helper.file_prefix(), APP_VERSION, helper_short_hash(helper), env::consts::EXE_SUFFIX))
}

fn metadata_path_for(helper_path: &Path) -> PathBuf {
    let mut path = helper_path.as_os_str().to_owned();
    path.push(".json");
    PathBuf::from(path)
}

fn helper_short_hash(helper: EmbeddedHelper) -> String {
    helper_sha256(helper)[..12].to_string()
}

fn helper_sha256(helper: EmbeddedHelper) -> String {
    let digest = Sha256::digest(helper.payload);
    format!("{:x}", digest)
}

fn current_helper_paths(cache_dir: &Path) -> Vec<PathBuf> {
    [HelperKind::Luac32, HelperKind::Luac64, HelperKind::Mklfs]
        .iter()
        .map(|kind| helper_for_kind(*kind))
        .flat_map(|helper| {
            let target = helper_target_path(cache_dir, helper);
            [target.clone(), metadata_path_for(&target)]
        })
        .collect()
}

fn run_cleanup_once(cache_dir: &Path) {
    CLEANUP_ONCE.call_once(|| {
        let keep = current_helper_paths(cache_dir);
        if let Err(err) = cleanup_stale_helpers(cache_dir, &keep) {
            log::debug!("failed to clean stale Lua helpers in {}: {}", cache_dir.display(), err);
        }
    });
}

fn cleanup_stale_helpers(cache_dir: &Path, keep: &[PathBuf]) -> io::Result<()> {
    let entries = match fs::read_dir(cache_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if !name.starts_with("luac32_helper-") && !name.starts_with("luac64_helper-") && !name.starts_with("mklfs_helper-") {
            continue;
        }
        if keep.iter().any(|keep_path| keep_path == &path) {
            continue;
        }
        if let Err(err) = fs::remove_file(&path) {
            log::debug!("failed to remove stale Lua helper {}: {}", path.display(), err);
        }
    }

    Ok(())
}

fn helper_file_is_current(helper_path: &Path, metadata_path: &Path, helper: EmbeddedHelper, expected_sha: &str) -> Result<bool, String> {
    if !helper_path.is_file() || !metadata_path.is_file() {
        return Ok(false);
    }

    let metadata = read_metadata(metadata_path)?;
    if metadata.app_version != APP_VERSION || metadata.sha256 != expected_sha {
        return Ok(false);
    }
    // Check name if present; fall back to bitness for old metadata files
    if !metadata.helper_name.is_empty() && metadata.helper_name != helper.name() {
        return Ok(false);
    }

    let bytes = fs::read(helper_path).map_err(|e| format!("failed to read cached {} helper at {}: {}", helper.name(), helper_path.display(), e))?;
    let actual_sha = format!("{:x}", Sha256::digest(&bytes));
    Ok(actual_sha == expected_sha)
}

fn read_metadata(path: &Path) -> Result<HelperMetadata, String> {
    let raw = fs::read(path).map_err(|e| format!("failed to read helper metadata at {}: {}", path.display(), e))?;
    serde_json::from_slice(&raw).map_err(|e| format!("failed to parse helper metadata at {}: {}", path.display(), e))
}

fn write_helper_files(cache_dir: &Path, helper_path: &Path, metadata_path: &Path, helper: EmbeddedHelper, expected_sha: &str) -> Result<(), String> {
    let temp_helper = cache_dir.join(format!(
        ".{}-{}-{}.tmp{}",
        helper.file_prefix(),
        std::process::id(),
        TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed),
        env::consts::EXE_SUFFIX
    ));
    let temp_metadata = metadata_path_for(&temp_helper);

    fs::write(&temp_helper, helper.payload).map_err(|e| format!("failed to write {} helper to {}: {}", helper.name(), temp_helper.display(), e))?;
    set_helper_permissions(&temp_helper).map_err(|e| format!("failed to set permissions on {} helper at {}: {}", helper.name(), temp_helper.display(), e))?;

    let metadata = HelperMetadata {
        app_version: APP_VERSION.to_string(),
        helper_name: helper.name().to_string(),
        sha256: expected_sha.to_string(),
        helper_bitness: 0,
    };
    let metadata_bytes = serde_json::to_vec_pretty(&metadata).map_err(|e| format!("failed to serialize helper metadata: {}", e))?;
    fs::write(&temp_metadata, metadata_bytes).map_err(|e| format!("failed to write helper metadata to {}: {}", temp_metadata.display(), e))?;

    persist_if_missing(&temp_helper, helper_path).map_err(|e| format!("failed to persist {} helper to {}: {}", helper.name(), helper_path.display(), e))?;
    persist_if_missing(&temp_metadata, metadata_path).map_err(|e| format!("failed to persist helper metadata to {}: {}", metadata_path.display(), e))?;

    Ok(())
}

fn persist_if_missing(temp_path: &Path, final_path: &Path) -> io::Result<()> {
    match fs::rename(temp_path, final_path) {
        Ok(()) => Ok(()),
        Err(_) if final_path.exists() => {
            let _ = fs::remove_file(temp_path);
            Ok(())
        }
        Err(err) => {
            let _ = fs::remove_file(temp_path);
            Err(err)
        }
    }
}

fn set_helper_permissions(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    let _ = path; // suppress unused on windows
    Ok(())
}

fn remove_if_exists(path: &Path) {
    if let Err(err) = fs::remove_file(path) {
        if err.kind() != io::ErrorKind::NotFound {
            log::debug!("failed to remove {}: {}", path.display(), err);
        }
    }
}
