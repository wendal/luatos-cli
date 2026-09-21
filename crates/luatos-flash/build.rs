// 定义统一 cfg `sf32lb` 用于门控 SF32LB 刷机代码。
//
// 注意：luatos-flash 的刷机实现目前固定使用 ChipType::SF32LB58（luatos-soc 把
// sf32lb52~58 全部映射到 Sf32lb58 族），因此刷机代码仅在 sf32lb58 feature 开启时编译。
// sf32lb52/55/56/57 feature 只内嵌对应型号的 RAM stub 数据（供外部工具使用），
// 不单独启用刷机代码。
fn main() {
    println!("cargo::rustc-check-cfg=cfg(sf32lb)");
    if std::env::var_os("CARGO_FEATURE_SF32LB58").is_some() {
        println!("cargo::rustc-cfg=sf32lb");
    }
}
