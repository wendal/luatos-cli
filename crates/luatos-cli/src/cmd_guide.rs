use crate::{event, OutputFormat};

pub struct ModelGuide {
    pub key: &'static str,
    pub aliases: &'static [&'static str],
    pub description: &'static str,
    pub flash_example: &'static str,
    pub log_example: &'static str,
    pub docs_path: &'static str,
}

const GUIDES: &[ModelGuide] = &[
    ModelGuide {
        key: "air1601",
        aliases: &["air1601", "air1602", "ccm4211"],
        description: "Air1601/Air1602/CCM4211（SOC 二进制日志，建议 --probe）",
        flash_example: "luatos-cli flash run --soc firmware.soc --port COM10",
        log_example: "luatos-cli log view-binary --port COM10 --baud 6000000 --probe",
        docs_path: "docs\\models\\air1601-air1602.md",
    },
    ModelGuide {
        key: "air8000",
        aliases: &["air8000", "ec7xx", "air780epm", "air780ehm", "air780ehv", "air780ehg"],
        description: "Air8000/EC7xx（USB CDC，日志建议 921600，--probe）",
        flash_example: "luatos-cli flash run --soc firmware.soc --port auto",
        log_example: "luatos-cli log view-binary --port auto --probe",
        docs_path: "docs\\models\\air8000-ec7xx.md",
    },
    ModelGuide {
        key: "air8101",
        aliases: &["air8101", "bk72xx"],
        description: "Air8101/BK7258（文本日志）",
        flash_example: "luatos-cli flash run --soc firmware.soc --port COM6",
        log_example: "luatos-cli log view --port COM6 --baud 921600",
        docs_path: "docs\\models\\air8101-bk72xx.md",
    },
    ModelGuide {
        key: "air6208",
        aliases: &["air6208", "air101", "air103", "air601", "xt804"],
        description: "Air6208/Air101/Air103（XT804 系列）",
        flash_example: "luatos-cli flash run --soc firmware.soc --port COM7",
        log_example: "luatos-cli log view-binary --port COM7 --baud 2000000",
        docs_path: "docs\\models\\air6208-xt804.md",
    },
    ModelGuide {
        key: "sf32",
        aliases: &["sf32", "sf32lb58", "air8101-sf32"],
        description: "SF32LB58（支持 --auto-reset）",
        flash_example: "luatos-cli flash run --soc firmware.soc --port COM13 --auto-reset",
        log_example: "luatos-cli log view --port COM13 --baud 1000000",
        docs_path: "docs\\models\\sf32lb58.md",
    },
];

fn normalize(input: &str) -> String {
    input.trim().to_ascii_lowercase().replace('_', "-")
}

pub fn find_model_guide(model: &str) -> Option<&'static ModelGuide> {
    let normalized = normalize(model);
    GUIDES.iter().find(|g| g.aliases.iter().any(|alias| normalize(alias) == normalized))
}

pub fn cmd_guide_models(format: &OutputFormat) -> anyhow::Result<()> {
    match format {
        OutputFormat::Text => {
            println!("型号二级帮助入口：");
            for guide in GUIDES {
                println!("- {}: {}", guide.key, guide.description);
                println!("  flash: {}", guide.flash_example);
                println!("  log:   {}", guide.log_example);
                println!("  docs:  {}", guide.docs_path);
            }
            Ok(())
        }
        OutputFormat::Json | OutputFormat::Jsonl => {
            let data: Vec<serde_json::Value> = GUIDES
                .iter()
                .map(|g| {
                    serde_json::json!({
                        "model": g.key,
                        "aliases": g.aliases,
                        "description": g.description,
                        "flash_example": g.flash_example,
                        "log_example": g.log_example,
                        "docs": g.docs_path,
                    })
                })
                .collect();
            event::emit_result(format, "guide.models", "ok", serde_json::json!({ "models": data }))
        }
    }
}

pub fn cmd_guide_model(model: &str, format: &OutputFormat) -> anyhow::Result<()> {
    let Some(guide) = find_model_guide(model) else {
        anyhow::bail!("未识别型号: {model}. 可先执行 `luatos-cli guide models` 查看支持列表");
    };

    match format {
        OutputFormat::Text => {
            println!("型号: {}", guide.key);
            println!("说明: {}", guide.description);
            println!("刷机: {}", guide.flash_example);
            println!("日志: {}", guide.log_example);
            println!("文档: {}", guide.docs_path);
            Ok(())
        }
        OutputFormat::Json | OutputFormat::Jsonl => event::emit_result(
            format,
            "guide.model",
            "ok",
            serde_json::json!({
                "model": guide.key,
                "aliases": guide.aliases,
                "description": guide.description,
                "flash_example": guide.flash_example,
                "log_example": guide.log_example,
                "docs": guide.docs_path,
            }),
        ),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn guide_alias_air1602_maps_to_air1601_family() {
        let guide = super::find_model_guide("Air1602").expect("expected guide");
        assert_eq!(guide.key, "air1601");
    }

    #[test]
    fn guide_lookup_is_case_insensitive() {
        let guide = super::find_model_guide("air8000").expect("expected guide");
        assert_eq!(guide.key, "air8000");
    }
}
