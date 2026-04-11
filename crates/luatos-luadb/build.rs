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

fn build_lua_helper(lua_src: &str, out_dir: &Path, bitw: u32, target_os: &str) {
    let mut build = cc::Build::new();
    build.include(lua_src).warnings(false);
    if bitw == 32 {
        build.define("LUA_32BITS", None);
    }
    apply_os_defines(&mut build, target_os);

    let compiler = build.get_compiler();
    let helper_src = PathBuf::from("csrc/luac_helper.c");
    let output = out_dir.join(format!("luac{}_helper{}", bitw, env::consts::EXE_SUFFIX));

    let mut cmd = compiler.to_command();
    for file in core_files(lua_src) {
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

    println!(
        "cargo:rustc-env=LUA53_HELPER_EMBED_{}={}",
        bitw,
        output.display()
    );
}

fn main() {
    let lua_src = "lua-5.3.6/src";
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set"));

    build_lua_helper(lua_src, &out_dir, 32, &target_os);
    build_lua_helper(lua_src, &out_dir, 64, &target_os);

    println!("cargo:rerun-if-changed=lua-5.3.6/src");
    println!("cargo:rerun-if-changed=csrc/luac_helper.c");
}
