use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn core_files(lua_src: &Path) -> Vec<PathBuf> {
    [
        "lapi.c",
        "lauxlib.c",
        "lbaselib.c",
        "lbitlib.c",
        "lcode.c",
        "lcorolib.c",
        "lctype.c",
        "ldblib.c",
        "ldebug.c",
        "ldo.c",
        "ldump.c",
        "lfunc.c",
        "lgc.c",
        "linit.c",
        "liolib.c",
        "llex.c",
        "lmathlib.c",
        "lmem.c",
        "loadlib.c",
        "lobject.c",
        "lopcodes.c",
        "loslib.c",
        "lparser.c",
        "lstate.c",
        "lstring.c",
        "lstrlib.c",
        "ltable.c",
        "ltablib.c",
        "ltm.c",
        "lundump.c",
        "lutf8lib.c",
        "lvm.c",
        "lzio.c",
    ]
    .iter()
    .map(|file| lua_src.join(file))
    .collect()
}

fn is_ident_start(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphabetic()
}

fn is_ident_continue(byte: u8) -> bool {
    is_ident_start(byte) || byte.is_ascii_digit()
}

// 两个 Lua 核心必须用不同符号前缀，避免 64 位 wrapper 静默链接到 32 位实现。
// Lua 自己定义的宏不能改名，否则 #define 不会同步替换宏名，include guard 也会失效。
// 若上游以后新增与宏同名的函数，该函数不会加前缀，链接期会以重复符号暴露问题。
fn lua_identifiers(lua_src: &Path) -> BTreeSet<String> {
    let mut identifiers = BTreeSet::new();
    let mut defined_macros = BTreeSet::new();
    let entries = fs::read_dir(lua_src).expect("failed to read Lua source directory");
    for entry in entries {
        let path = entry.expect("failed to read Lua source entry").path();
        if !matches!(path.extension().and_then(|ext| ext.to_str()), Some("c" | "h")) {
            continue;
        }
        let content = fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
        for line in content.lines() {
            if let Some(rest) = line.trim_start().strip_prefix("#define") {
                let bytes = rest.trim_start().as_bytes();
                let end = bytes.iter().take_while(|byte| is_ident_continue(**byte)).count();
                if end > 0 {
                    defined_macros.insert(String::from_utf8(bytes[..end].to_vec()).unwrap());
                }
            }
        }
        let bytes = content.as_bytes();
        let mut start = 0;
        while start < bytes.len() {
            if !is_ident_start(bytes[start]) || (start > 0 && is_ident_continue(bytes[start - 1])) {
                start += 1;
                continue;
            }
            let mut end = start + 1;
            while end < bytes.len() && is_ident_continue(bytes[end]) {
                end += 1;
            }
            let name = &content[start..end];
            if name.starts_with("lua") {
                identifiers.insert(name.to_owned());
            }
            start = end;
        }
    }
    identifiers.retain(|name| !defined_macros.contains(name));
    identifiers
}

fn write_symbol_prefix_header(lua_src: &Path, out_dir: &Path, bitw: u32) -> PathBuf {
    let path = out_dir.join(format!("luac{bitw}_symbols.h"));
    let guard = format!("LUATOS_LUAC{bitw}_SYMBOLS_H");
    let mut content = format!("#ifndef {guard}\n#define {guard}\n");
    for name in lua_identifiers(lua_src) {
        content.push_str(&format!("#define {name} luac{bitw}_{name}\n"));
    }
    content.push_str("#endif\n");
    fs::write(&path, content).unwrap_or_else(|e| panic!("failed to write {}: {e}", path.display()));
    path
}

fn apply_os_defines(build: &mut cc::Build, target_os: &str) {
    match target_os {
        "macos" => {
            build.define("LUA_USE_MACOSX", None);
        }
        "linux" => {
            build.define("LUA_USE_LINUX", None);
        }
        _ => {}
    }
}

fn build_lua_compiler(lua_src: &Path, out_dir: &Path, bitw: u32, target_os: &str) {
    let symbols = write_symbol_prefix_header(lua_src, out_dir, bitw);
    let mut build = cc::Build::new();
    build.out_dir(out_dir.join(format!("luac{bitw}")));
    // 静态库不需要调试信息；MSVC 的 -Z7/-Brepro 组合在部分环境会触发 cl 的 D8050。
    build.debug(false);
    build.include(lua_src).warnings(false);
    // 把编译器临时文件固定到 OUT_DIR，规避部分 Windows/MSVC 环境的 D8050/D8018。
    build.env("TMP", out_dir);
    build.env("TEMP", out_dir);
    if bitw == 32 {
        build.define("LUA_32BITS", None);
    }
    apply_os_defines(&mut build, target_os);
    if build.get_compiler().is_like_msvc() {
        build.flag(format!("/FI{}", symbols.display()));
    } else {
        build.flag("-include").flag(symbols.as_os_str());
    }

    let helper_src = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set")).join("csrc/luac_helper.c");
    for file in core_files(lua_src) {
        build.file(file);
    }
    build.file(&helper_src);
    build.compile(&format!("luatos_luac{bitw}"));
}

fn build_mklfs(lfs_src: &Path, out_dir: &Path) {
    let mut build = cc::Build::new();
    build.out_dir(out_dir.join("mklfs"));
    build.debug(false);
    build.include(lfs_src).warnings(false);
    build.env("TMP", out_dir);
    build.env("TEMP", out_dir);
    // Disable LFS tracing/debug in release builds
    build.define("LFS_NO_DEBUG", None);
    build.define("LFS_NO_WARN", None);
    build.define("LFS_NO_ERROR", None);

    let builder_src = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set")).join("csrc/mklfs.c");
    build.file(builder_src);
    build.file(lfs_src.join("lfs.c"));
    build.file(lfs_src.join("lfs_util.c"));
    build.compile("luatos_mklfs");
}

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set"));
    let lua_src = manifest_dir.join("lua-5.3.6/src");
    let lfs_src = manifest_dir.join("lfs");
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set"));

    build_lua_compiler(&lua_src, &out_dir, 32, &target_os);
    build_lua_compiler(&lua_src, &out_dir, 64, &target_os);
    build_mklfs(&lfs_src, &out_dir);
    if target_os == "linux" {
        println!("cargo:rustc-link-lib=dylib=m");
        println!("cargo:rustc-link-lib=dylib=dl");
    }

    println!("cargo:rerun-if-changed=lua-5.3.6/src");
    println!("cargo:rerun-if-changed=csrc/luac_helper.c");
    println!("cargo:rerun-if-changed=csrc/mklfs.c");
    println!("cargo:rerun-if-changed=lfs");
}
