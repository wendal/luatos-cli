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

fn build_lua_helper(lua_src: &Path, out_dir: &Path, bitw: u32, target_os: &str) {
    let mut build = cc::Build::new();
    build.include(lua_src).warnings(false);
    if bitw == 32 {
        build.define("LUA_32BITS", None);
    }
    apply_os_defines(&mut build, target_os);

    let compiler = build.get_compiler();
    let helper_src = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set")).join("csrc/luac_helper.c");
    let output = out_dir.join(format!("luac{}_helper{}", bitw, env::consts::EXE_SUFFIX));

    let mut cmd = compiler.to_command();
    // cl.exe 会把中间 .obj 写到当前目录；切到 OUT_DIR 避免污染源码目录
    cmd.current_dir(out_dir);
    for file in core_files(&lua_src.to_string_lossy()) {
        cmd.arg(file);
    }
    cmd.arg(&helper_src);
    cmd.arg("-I").arg(lua_src);
    if bitw == 32 {
        cmd.arg("-DLUA_32BITS");
    }
    apply_os_args(&mut cmd, target_os);
    cmd.arg("-o").arg(&output);

    let status = cmd.status().expect("Failed to spawn Lua helper compiler");
    if !status.success() {
        panic!("Failed to build Lua {}-bit helper", bitw);
    }

    println!("cargo:rustc-env=LUA53_HELPER_EMBED_{}={}", bitw, output.display());
}

fn build_mklfs_helper(lfs_src: &Path, out_dir: &Path, target_os: &str) {
    let mut build = cc::Build::new();
    build.include(lfs_src).warnings(false);
    // Disable LFS tracing/debug in release builds
    build.define("LFS_NO_DEBUG", None);
    build.define("LFS_NO_WARN", None);
    build.define("LFS_NO_ERROR", None);

    let compiler = build.get_compiler();
    let helper_src = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set")).join("csrc/mklfs_helper.c");
    let lfs_c = lfs_src.join("lfs.c");
    let lfs_util_c = lfs_src.join("lfs_util.c");
    let output = out_dir.join(format!("mklfs_helper{}", env::consts::EXE_SUFFIX));

    let mut cmd = compiler.to_command();
    // cl.exe 会把中间 .obj 写到当前目录；切到 OUT_DIR 避免污染源码目录
    cmd.current_dir(out_dir);
    cmd.arg(&helper_src);
    cmd.arg(&lfs_c);
    cmd.arg(&lfs_util_c);
    cmd.arg("-I").arg(lfs_src);
    cmd.arg("-DLFS_NO_DEBUG");
    cmd.arg("-DLFS_NO_WARN");
    cmd.arg("-DLFS_NO_ERROR");
    if target_os == "linux" {
        cmd.arg("-lm");
    }
    cmd.arg("-o").arg(&output);

    let status = cmd.status().expect("Failed to spawn mklfs helper compiler");
    if !status.success() {
        panic!("Failed to build mklfs helper");
    }

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
