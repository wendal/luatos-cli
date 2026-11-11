use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn core_files(lua_src: &str) -> Vec<String> {
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
    .map(|file| format!("{}/{}", lua_src, file))
    .collect()
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

fn apply_os_args(cmd: &mut Command, target_os: &str) {
    match target_os {
        "macos" => {
            cmd.arg("-DLUA_USE_MACOSX");
        }
        "linux" => {
            cmd.arg("-DLUA_USE_LINUX");
            cmd.arg("-lm");
            cmd.arg("-ldl");
        }
        _ => {}
    }
}

fn link_helper(build: &cc::Build, out_dir: &Path, output: &Path, target_os: &str, label: &str) {
    // 先逐文件编译成 .obj/.o，再链接。MSVC 的 cl.exe 在单条命令里塞入
    // 大量 Lua 源文件时可能触发 D8050（命令行无法写入调试记录）；拆成
    // “逐文件编译 + 短链接命令”后不再受命令行长度影响。
    let objects = build.try_compile_intermediates().unwrap_or_else(|e| panic!("Failed to compile {label}: {e}"));

    let compiler = build.get_compiler();
    let mut cmd = compiler.to_command();
    cmd.current_dir(out_dir);
    cmd.env("TMP", out_dir);
    cmd.env("TEMP", out_dir);
    for object in &objects {
        cmd.arg(object);
    }
    if compiler.is_like_msvc() {
        cmd.arg(format!("/Fe:{}", output.display()));
    } else {
        cmd.arg("-o").arg(output);
        apply_os_args(&mut cmd, target_os);
        if target_os == "windows" && label == "mklfs helper" {
            cmd.arg("-municode");
        }
    }

    let status = cmd.status().unwrap_or_else(|e| panic!("Failed to spawn {label} linker: {e}"));
    if !status.success() {
        panic!("Failed to link {label}");
    }
}

fn build_lua_helper(lua_src: &Path, out_dir: &Path, bitw: u32, target_os: &str) {
    let mut build = cc::Build::new();
    // 辅助可执行文件不需要调试信息；MSVC 的 -Z7/-Brepro 组合在部分环境会触发
    // cl 的 D8050（无法把命令行写入调试记录）。
    build.debug(false);
    build.include(lua_src).warnings(false);
    // 把编译器/链接器的临时文件固定到 OUT_DIR：部分 Windows 环境对系统 TEMP
    // 下的 cl 调试记录/链接响应文件创建失败（D8050/D8018）。
    build.env("TMP", out_dir);
    build.env("TEMP", out_dir);
    if bitw == 32 {
        build.define("LUA_32BITS", None);
    }
    apply_os_defines(&mut build, target_os);

    let helper_src = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set")).join("csrc/luac_helper.c");
    for file in core_files(&lua_src.to_string_lossy()) {
        build.file(file);
    }
    build.file(&helper_src);

    let output = out_dir.join(format!("luac{}_helper{}", bitw, env::consts::EXE_SUFFIX));
    link_helper(&build, out_dir, &output, target_os, &format!("Lua {bitw}-bit helper"));

    println!("cargo:rustc-env=LUA53_HELPER_EMBED_{}={}", bitw, output.display());
}

fn build_mklfs_helper(lfs_src: &Path, out_dir: &Path, target_os: &str) {
    let mut build = cc::Build::new();
    build.debug(false);
    build.include(lfs_src).warnings(false);
    build.env("TMP", out_dir);
    build.env("TEMP", out_dir);
    // Disable LFS tracing/debug in release builds
    build.define("LFS_NO_DEBUG", None);
    build.define("LFS_NO_WARN", None);
    build.define("LFS_NO_ERROR", None);

    let helper_src = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set")).join("csrc/mklfs_helper.c");
    build.file(helper_src);
    build.file(lfs_src.join("lfs.c"));
    build.file(lfs_src.join("lfs_util.c"));

    let output = out_dir.join(format!("mklfs_helper{}", env::consts::EXE_SUFFIX));
    link_helper(&build, out_dir, &output, target_os, "mklfs helper");

    println!("cargo:rustc-env=MKLFS_HELPER_EMBED={}", output.display());
}

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set"));
    let lua_src = manifest_dir.join("lua-5.3.6/src");
    let lfs_src = manifest_dir.join("lfs");
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set"));

    build_lua_helper(&lua_src, &out_dir, 32, &target_os);
    build_lua_helper(&lua_src, &out_dir, 64, &target_os);
    build_mklfs_helper(&lfs_src, &out_dir, &target_os);

    println!("cargo:rerun-if-changed=lua-5.3.6/src");
    println!("cargo:rerun-if-changed=csrc/luac_helper.c");
    println!("cargo:rerun-if-changed=csrc/mklfs_helper.c");
    println!("cargo:rerun-if-changed=lfs");
}
