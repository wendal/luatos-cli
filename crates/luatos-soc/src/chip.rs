use std::fmt;

/// 芯片族：从 info.json 的 chip_type 字符串归一化而来，作为全仓库分发决策的单一来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChipFamily {
    Bk72xx,   // Air8101
    Xt804,    // Air6208 / Air101 / Air103 / Air601
    Ccm4211,  // Air1601 / Air1602
    Ec718,    // Air8000 / Air780E* / EC7xx / EC618
    Sf32lb58, // SF32LB5x
    Rda8910,  // Air724UG / UIS8910DM
    Air6201,  // 外置 SPI Flash
    Unknown,  // 未识别
}

impl ChipFamily {
    /// 从 chip_type 字符串归一化出芯片族（大小写不敏感）。
    pub fn from_chip_type(s: &str) -> ChipFamily {
        match s.to_ascii_lowercase().as_str() {
            "bk72xx" | "air8101" => ChipFamily::Bk72xx,
            "air6208" | "air101" | "air103" | "air601" => ChipFamily::Xt804,
            "air1601" | "air1602" | "ccm4211" => ChipFamily::Ccm4211,
            "ec7xx" | "ec618" | "ec718" | "air8000" | "air780epm" | "air780ehm" | "air780ehv" | "air780ehg" | "air780epv" | "air8000m" => ChipFamily::Ec718,
            "sf32lb58" | "sf32lb52" | "sf32lb55" | "sf32lb56" | "sf32lb57" => ChipFamily::Sf32lb58,
            "uis8910" | "rda8910" | "air724ug" | "8910dm" => ChipFamily::Rda8910,
            "air6201" => ChipFamily::Air6201,
            _ => ChipFamily::Unknown,
        }
    }

    /// 是否为 EC718 家族（Air8000 / Air780E* / EC7xx / EC618）。
    pub fn is_ec718(self) -> bool {
        matches!(self, ChipFamily::Ec718)
    }

    /// 是否使用二进制日志（CCM4211 与 EC718 家族）。
    pub fn uses_binary_log(self) -> bool {
        matches!(self, ChipFamily::Ccm4211 | ChipFamily::Ec718)
    }

    /// 返回显示名（与 info.json 的 chip_type 常用写法一致）。
    pub fn name(self) -> &'static str {
        match self {
            ChipFamily::Bk72xx => "bk72xx",
            ChipFamily::Xt804 => "xt804",
            ChipFamily::Ccm4211 => "ccm4211",
            ChipFamily::Ec718 => "ec7xx",
            ChipFamily::Sf32lb58 => "sf32lb58",
            ChipFamily::Rda8910 => "rda8910",
            ChipFamily::Air6201 => "air6201",
            ChipFamily::Unknown => "unknown",
        }
    }
}

impl fmt::Display for ChipFamily {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SocInfo;

    /// 每种族的代表名映射到正确变体。
    #[test]
    fn known_names_map_to_variants() {
        assert_eq!(ChipFamily::from_chip_type("bk72xx"), ChipFamily::Bk72xx);
        assert_eq!(ChipFamily::from_chip_type("air8101"), ChipFamily::Bk72xx);
        assert_eq!(ChipFamily::from_chip_type("air6208"), ChipFamily::Xt804);
        assert_eq!(ChipFamily::from_chip_type("air101"), ChipFamily::Xt804);
        assert_eq!(ChipFamily::from_chip_type("air103"), ChipFamily::Xt804);
        assert_eq!(ChipFamily::from_chip_type("air601"), ChipFamily::Xt804);
        assert_eq!(ChipFamily::from_chip_type("air1601"), ChipFamily::Ccm4211);
        assert_eq!(ChipFamily::from_chip_type("air1602"), ChipFamily::Ccm4211);
        assert_eq!(ChipFamily::from_chip_type("ccm4211"), ChipFamily::Ccm4211);
        assert_eq!(ChipFamily::from_chip_type("ec7xx"), ChipFamily::Ec718);
        assert_eq!(ChipFamily::from_chip_type("ec618"), ChipFamily::Ec718);
        assert_eq!(ChipFamily::from_chip_type("air8000"), ChipFamily::Ec718);
        assert_eq!(ChipFamily::from_chip_type("air8000m"), ChipFamily::Ec718);
        assert_eq!(ChipFamily::from_chip_type("air780epm"), ChipFamily::Ec718);
        assert_eq!(ChipFamily::from_chip_type("air780ehv"), ChipFamily::Ec718);
        assert_eq!(ChipFamily::from_chip_type("air780ehg"), ChipFamily::Ec718);
        assert_eq!(ChipFamily::from_chip_type("air780epv"), ChipFamily::Ec718);
        assert_eq!(ChipFamily::from_chip_type("sf32lb58"), ChipFamily::Sf32lb58);
        assert_eq!(ChipFamily::from_chip_type("sf32lb52"), ChipFamily::Sf32lb58);
        assert_eq!(ChipFamily::from_chip_type("uis8910"), ChipFamily::Rda8910);
        assert_eq!(ChipFamily::from_chip_type("rda8910"), ChipFamily::Rda8910);
        assert_eq!(ChipFamily::from_chip_type("air724ug"), ChipFamily::Rda8910);
        assert_eq!(ChipFamily::from_chip_type("8910dm"), ChipFamily::Rda8910);
        assert_eq!(ChipFamily::from_chip_type("air6201"), ChipFamily::Air6201);
    }

    /// 大小写不敏感。
    #[test]
    fn matching_is_case_insensitive() {
        assert_eq!(ChipFamily::from_chip_type("AIR8101"), ChipFamily::Bk72xx);
        assert_eq!(ChipFamily::from_chip_type("Bk72xx"), ChipFamily::Bk72xx);
        assert_eq!(ChipFamily::from_chip_type("Ec7XX"), ChipFamily::Ec718);
    }

    /// 未知名映射到 Unknown。
    #[test]
    fn unknown_name_maps_to_unknown() {
        assert_eq!(ChipFamily::from_chip_type("unknown_chip"), ChipFamily::Unknown);
        assert_eq!(ChipFamily::from_chip_type(""), ChipFamily::Unknown);
    }

    /// air8000m 必须归一化为 Ec718（修复 air8000m 不一致的关键断言）。
    #[test]
    fn air8000m_is_ec718() {
        assert!(ChipFamily::from_chip_type("air8000m").is_ec718());
    }

    /// uses_binary_log 正反例。
    #[test]
    fn uses_binary_log_cases() {
        assert!(ChipFamily::Ccm4211.uses_binary_log());
        assert!(ChipFamily::Ec718.uses_binary_log());
        assert!(!ChipFamily::Bk72xx.uses_binary_log());
        assert!(!ChipFamily::Xt804.uses_binary_log());
        assert!(!ChipFamily::Sf32lb58.uses_binary_log());
        assert!(!ChipFamily::Rda8910.uses_binary_log());
        assert!(!ChipFamily::Air6201.uses_binary_log());
        assert!(!ChipFamily::Unknown.uses_binary_log());
    }

    /// Display 与 name() 一致（转发）。
    #[test]
    fn display_matches_name() {
        assert_eq!(ChipFamily::Ec718.name(), "ec7xx");
        assert_eq!(ChipFamily::Ec718.to_string(), "ec7xx");
        assert_eq!(ChipFamily::Unknown.to_string(), "unknown");
        assert_eq!(ChipFamily::Sf32lb58.to_string(), "sf32lb58");
        assert_eq!(ChipFamily::Rda8910.to_string(), "rda8910");
    }

    /// family() 基于 chip.chip_type 归一化。
    #[test]
    fn family_derives_from_chip_type() {
        let info: SocInfo = serde_json::from_str(
            r#"{
                "version": 1,
                "chip": {"type": "air8000m"},
                "rom": {"file": "AIR8000M.bin"},
                "script": {"file": "script.img"},
                "download": {}
            }"#,
        )
        .unwrap();
        assert_eq!(info.family(), ChipFamily::Ec718);
        assert!(info.family().is_ec718());
    }
}
