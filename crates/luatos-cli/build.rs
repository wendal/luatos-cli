// 定义统一 cfg `sf32lb` 用于门控 luatos-cli 中的 SF32LB 刷机功能引用，
// 与 luatos-flash 的 build.rs 保持同一语义：仅 sf32lb58 feature 开启时启用刷机代码
// （luatos-soc 把 sf32lb52~58 全部映射到 Sf32lb58 族，刷机实现固定使用 SF32LB58）。
fn main() {
    println!("cargo::rustc-check-cfg=cfg(sf32lb)");
    if std::env::var_os("CARGO_FEATURE_SF32LB58").is_some() {
        println!("cargo::rustc-cfg=sf32lb");
    }
}
