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
        key: "ec716s",
        aliases: &["ec716s", "air780el", "air780et"],
        description: "EC716S / Air780EL / Air780ET（USB 原生刷写 AT binpkg，运行后通过 AT 口确认版本）",
        flash_example: "luatos-cli flash run --binpkg firmware.binpkg --port auto",
        log_example: "luatos-cli serial list",
        docs_path: "docs\\models\\ec716s.md",
    },
    ModelGuide {
        key: "air1601",
        aliases: &["air1601", "air1602", "air1603", "air1604", "ccm4211"],
        description: "Air1601/Air1602/Air1603/Air1604/CCM4211（SOC 二进制日志，建议 --probe）",
        flash_example: "luatos-cli flash run --soc firmware.soc --port COM10",
        log_example: "luatos-cli log view-binary --port COM10 --baud 6000000 --probe",
        docs_path: "docs\\models\\air1601-air1604.md",
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
        key: "ec618",
        aliases: &["ec618", "air780e", "air780eg"],
        description: "EC618/Air780E/Air780EG（独立 CP Flash；免 BOOT 失败时按住 BOOT 复位）",
        flash_example: "luatos-cli flash run --soc firmware.soc --port auto",
        log_example: "luatos-cli log view-binary --port auto --baud 921600 --probe",
        docs_path: "docs\\models\\ec618.md",
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
        key: "d126",
        aliases: &["air8609w", "d126", "d126fcr", "d12p", "artinchip"],
        description: "Air8609W（D126/D12P，ArtInChip；支持 .soc、USB 烧录、脚本合并与 LFTA FOTA）",
        flash_example: "luatos-cli flash run --soc LuatOS-SoC_V0001_Air8609W_1.soc --port auto",
        log_example: "luatos-cli log view --port COMx --baud 115200",
        docs_path: "docs\\models\\d126-d12p.md",
    },
];

const GUIDES_TAIL: &[ModelGuide] = &[ModelGuide {
    key: "air724ug",
    aliases: &["air724ug", "uis8910", "rda8910"],
    description: "Air724UG/UIS8910DM（文本日志，开发刷机仅 AP/PS/LUA）",
    flash_example: "luatos-cli flash run --soc firmware.soc --port COM12",
    log_example: "luatos-cli log view --port COM12 --baud 921600",
    docs_path: "docs\\models\\air724ug.md",
}];

fn all_guides() -> impl Iterator<Item = &'static ModelGuide> {
    GUIDES.iter().chain(GUIDES_TAIL)
}

fn normalize(input: &str) -> String {
    input.trim().to_ascii_lowercase().replace('_', "-")
}

pub fn find_model_guide(model: &str) -> Option<&'static ModelGuide> {
    let normalized = normalize(model);
    all_guides().find(|g| g.aliases.iter().any(|alias| normalize(alias) == normalized))
}

pub fn cmd_guide_models(format: &OutputFormat) -> anyhow::Result<()> {
    match format {
        OutputFormat::Text => {
            println!("型号二级帮助入口：");
            for guide in all_guides() {
                println!("- {}: {}", guide.key, guide.description);
                println!("  flash: {}", guide.flash_example);
                println!("  log:   {}", guide.log_example);
                println!("  docs:  {}", guide.docs_path);
            }
            Ok(())
        }
        OutputFormat::Json | OutputFormat::Jsonl => {
            let data: Vec<serde_json::Value> = all_guides()
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
    fn ec716s_el_et_share_binpkg_guide() {
        for name in ["EC716S", "Air780EL", "Air780ET"] {
            let guide = super::find_model_guide(name).unwrap();
            assert_eq!(guide.key, "ec716s");
            assert!(guide.flash_example.contains("--binpkg"));
        }
    }

    #[test]
    fn guide_alias_air1602_maps_to_air1601_family() {
        let guide = super::find_model_guide("Air1602").expect("expected guide");
        assert_eq!(guide.key, "air1601");
    }

    #[test]
    fn guide_alias_air1603_air1604_map_to_air1601_family() {
        for model in ["Air1603", "Air1604", "ccm4211"] {
            let guide = super::find_model_guide(model).expect("expected guide");
            assert_eq!(guide.key, "air1601", "{model} 应当归到 air1601 族");
        }
    }

    #[test]
    fn guide_lookup_is_case_insensitive() {
        let guide = super::find_model_guide("air8000").expect("expected guide");
        assert_eq!(guide.key, "air8000");
    }

    #[test]
    fn d126_aliases_share_guide() {
        for name in ["Air8609W", "air8609w", "D126", "d126fcr", "d12p", "artinchip"] {
            let guide = super::find_model_guide(name).expect("D126 应有型号指南");
            assert_eq!(guide.key, "d126");
            assert_eq!(guide.docs_path, "docs\\models\\d126-d12p.md");
        }
    }

    #[test]
    fn ec618_aliases_have_separate_model_guide() {
        for name in ["EC618", "Air780E", "air780eg"] {
            let guide = super::find_model_guide(name).expect("EC618 应有型号指南");
            assert_eq!(guide.key, "ec618");
            assert_eq!(guide.docs_path, "docs\\models\\ec618.md");
        }
    }
}
