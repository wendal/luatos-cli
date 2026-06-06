use std::path::Path;

/// 拷贝 C 头文件到 OUT_DIR, 方便本地开发时引用.
fn main() {
    let src = Path::new("include/luatos_log.h");
    let dst = Path::new(&std::env::var("OUT_DIR").unwrap()).join("luatos_log.h");
    // 头文件可能尚未生成 (在 src/lib.rs 之前创建), 不强制要求存在
    if src.exists() {
        std::fs::copy(src, dst).expect("failed to copy luatos_log.h to OUT_DIR");
    }
    println!("cargo:rerun-if-changed=include/luatos_log.h");
    println!("cargo:rerun-if-changed=Cargo.toml");
}
