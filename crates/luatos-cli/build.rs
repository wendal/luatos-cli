// 定义统一 cfg `sf32lb` 用于门控 luatos-cli 中的 SF32LB 刷机功能引用，
// 与 luatos-flash 的 build.rs 保持同一语义：仅 sf32lb58 feature 开启时启用刷机代码
// （luatos-soc 把 sf32lb52~58 全部映射到 Sf32lb58 族，刷机实现固定使用 SF32LB58）。
fn main() {
    println!("cargo::rustc-check-cfg=cfg(sf32lb)");
    if std::env::var_os("CARGO_FEATURE_SF32LB58").is_some() {
        println!("cargo::rustc-cfg=sf32lb");
    }
    bundle_ffmpeg().expect("复制随包 FFmpeg 失败");
}

/// 从本项目固定资源复制到产物旁；运行时只依赖发布目录，不引用源码路径。
fn bundle_ffmpeg() -> std::io::Result<()> {
    use std::path::PathBuf;

    let target = std::env::var("TARGET").expect("Cargo TARGET 未设置");
    let source = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../../vendor/ffmpeg").join(&target);
    println!("cargo::rerun-if-changed={}", source.display());
    println!("cargo::rerun-if-env-changed=LUATOS_REQUIRE_BUNDLED_FFMPEG");
    let binary = if target.contains("windows") { "bin/ffmpeg.exe" } else { "bin/ffmpeg" };
    if !source.join(binary).is_file() {
        let message = format!("缺少 {target} 的随包 FFmpeg，请先运行 python scripts/prepare_ffmpeg.py --target {target}");
        if std::env::var_os("LUATOS_REQUIRE_BUNDLED_FFMPEG").is_some() {
            return Err(std::io::Error::new(std::io::ErrorKind::NotFound, message));
        }
        println!("cargo::warning={message}");
        return Ok(());
    }
    // LFS 未拉取时不能将一百余字节的 pointer 误当成程序发布。
    if source.join(binary).metadata()?.len() < 1024 * 1024 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "FFmpeg 文件过短；请先拉取 Git LFS 资源或运行 prepare_ffmpeg.py",
        ));
    }
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let profile = out.ancestors().nth(3).expect("Cargo OUT_DIR 布局无效");
    let destination = profile.join("tools/ffmpeg");
    std::fs::create_dir_all(destination.join("bin"))?;
    for relative in [binary, "FFmpeg-LICENSE.txt", "FFmpeg-README.txt"] {
        std::fs::copy(source.join(relative), destination.join(relative))?;
    }
    Ok(())
}
