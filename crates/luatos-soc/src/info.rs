// info.json schema for SOC files.

use serde::{Deserialize, Serialize};

/// Top-level SOC metadata from info.json.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocInfo {
    pub version: Option<u32>,
    pub chip: SocChip,
    pub rom: SocRom,
    pub script: SocScript,
    pub download: SocDownload,
    pub user: Option<SocUser>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocChip {
    #[serde(rename = "type")]
    pub chip_type: String,
    pub ram: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocRom {
    pub file: String,
    pub fs: Option<SocFs>,
    #[serde(rename = "version-bsp")]
    pub version_bsp: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocFs {
    pub script: Option<SocScriptFs>,
    pub filesystem: Option<SocPartition>,
    pub kv: Option<SocPartition>,
    pub ap: Option<SocPartition>,
    pub fota: Option<SocPartition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocScriptFs {
    pub offset: Option<String>,
    pub size: Option<u64>,
    #[serde(rename = "type")]
    pub fs_type: Option<String>,
    pub bkcrc: Option<bool>,
    pub location: Option<String>,
}

/// Generic flash partition descriptor (filesystem, kv, fota, etc.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocPartition {
    pub offset: Option<String>,
    pub size: Option<u64>,
    #[serde(rename = "type")]
    pub fs_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocScript {
    pub file: String,
    pub lua: Option<String>,
    pub bitw: Option<u32>,
    #[serde(rename = "use-luac")]
    pub use_luac: Option<bool>,
    #[serde(rename = "use-debug")]
    pub use_debug: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocDownload {
    pub bl_addr: Option<String>,
    pub script_addr: Option<String>,
    pub force_br: Option<String>,
    pub cp_addr: Option<String>,
    pub ap_addr: Option<String>,
    // XT804 (Air6208) specific fields
    pub core_addr: Option<String>,
    pub app_addr: Option<String>,
    pub ota_addr: Option<String>,
    pub run_addr: Option<String>,
    pub user_addr: Option<String>,
    // CCM4211 (Air1601) specific fields
    pub fs_addr: Option<String>,
    pub fskv_addr: Option<String>,
    pub nvm_addr: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocUser {
    pub log_br: Option<String>,
    pub project: Option<String>,
    pub version: Option<String>,
}

impl SocInfo {
    /// Get the log baud rate, defaulting to 921600.
    pub fn log_baud_rate(&self) -> u32 {
        self.user
            .as_ref()
            .and_then(|u| u.log_br.as_deref())
            .and_then(|s| s.parse().ok())
            .unwrap_or(921_600)
    }

    /// Get the flash baud rate from download.force_br.
    /// Air6208 uses "2M" shorthand; Air8101 uses "2000000".
    pub fn flash_baud_rate(&self) -> u32 {
        self.download
            .force_br
            .as_deref()
            .and_then(parse_baud_rate)
            .unwrap_or(2_000_000)
    }

    /// Whether script partition uses BK CRC16 framing.
    pub fn use_bkcrc(&self) -> bool {
        self.rom
            .fs
            .as_ref()
            .and_then(|fs| fs.script.as_ref())
            .and_then(|s| s.bkcrc)
            .unwrap_or(false)
    }

    /// Get the script download address (ISP protocol address).
    pub fn script_addr(&self) -> u32 {
        parse_addr(self.download.script_addr.as_deref().unwrap_or("0x200000")).unwrap_or(0x200000)
            as u32
    }

    /// Get the script partition size in bytes.
    pub fn script_size(&self) -> usize {
        self.rom
            .fs
            .as_ref()
            .and_then(|fs| fs.script.as_ref())
            .and_then(|s| s.size)
            .unwrap_or(512) as usize
            * 1024
    }

    /// Get the filesystem partition address and size (bytes).
    pub fn filesystem_partition(&self) -> Option<(u32, usize)> {
        let fs = self.rom.fs.as_ref()?.filesystem.as_ref()?;
        let offset = parse_addr(fs.offset.as_deref()?)? as u32;
        let size = fs.size? as usize * 1024;
        Some((offset, size))
    }

    /// Get the FSKV (key-value) partition address and size (bytes).
    pub fn kv_partition(&self) -> Option<(u32, usize)> {
        let fs = self.rom.fs.as_ref()?.kv.as_ref()?;
        let offset = parse_addr(fs.offset.as_deref()?)? as u32;
        let size = fs.size? as usize * 1024;
        Some((offset, size))
    }

    /// Get the Lua bit width from the SOC, with chip-based defaults.
    /// BK72xx/Air8101 default to 32, XT804/Air6208/CCM4211 default to 64.
    pub fn script_bitw(&self) -> u32 {
        if let Some(bitw) = self.script.bitw {
            return bitw;
        }
        match self.chip.chip_type.as_str() {
            "air6208" | "air101" | "air103" | "air601" | "air1601" | "ccm4211" => 64,
            _ => 32,
        }
    }

    /// Get the bootloader address (CCM4211/Air1601).
    pub fn bl_addr(&self) -> Option<u32> {
        parse_addr(self.download.bl_addr.as_deref()?).map(|v| v as u32)
    }

    /// Get the application/core address.
    pub fn app_addr(&self) -> Option<u32> {
        parse_addr(self.download.app_addr.as_deref()?).map(|v| v as u32)
    }

    /// Get the filesystem partition address (CCM4211/Air1601).
    pub fn fs_addr(&self) -> Option<u32> {
        parse_addr(self.download.fs_addr.as_deref()?).map(|v| v as u32)
    }

    /// Get the NVM/FSKV partition address (CCM4211/Air1601).
    pub fn nvm_addr(&self) -> Option<u32> {
        parse_addr(self.download.nvm_addr.as_deref()?).map(|v| v as u32)
    }

    /// Whether Lua compilation is enabled (defaults to true).
    pub fn script_use_luac(&self) -> bool {
        self.script.use_luac.unwrap_or(true)
    }

    /// Whether to strip debug info (inverse of use-debug, defaults to false).
    pub fn script_strip_debug(&self) -> bool {
        self.script.use_debug.map(|d| !d).unwrap_or(false)
    }
}

/// Parse a flash address from info.json ("0x…" hex or bare hex digits).
pub fn parse_addr(s: &str) -> Option<u64> {
    let s = s.trim();
    let hex = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    u64::from_str_radix(hex, 16).ok()
}

/// Parse baud rate string, supporting shorthand like "2M", "1M", "115200".
pub fn parse_baud_rate(s: &str) -> Option<u32> {
    let s = s.trim();
    if let Some(m) = s.strip_suffix('M').or_else(|| s.strip_suffix('m')) {
        m.parse::<u32>().ok().map(|v| v * 1_000_000)
    } else if let Some(k) = s.strip_suffix('K').or_else(|| s.strip_suffix('k')) {
        k.parse::<u32>().ok().map(|v| v * 1_000)
    } else {
        s.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hex_addr() {
        assert_eq!(parse_addr("0x200000"), Some(0x200000));
        assert_eq!(parse_addr("0X0"), Some(0));
        assert_eq!(parse_addr("200000"), Some(0x200000));
    }

    #[test]
    fn parse_info_json() {
        let json = r#"{
            "version": 2013,
            "chip": {"type": "bk72xx"},
            "rom": {"file": "luatos.bin"},
            "script": {"file": "script.bin", "bitw": 64},
            "download": {"bl_addr": "0x0", "script_addr": "0x200000", "force_br": "2000000"},
            "user": {"log_br": "921600"}
        }"#;
        let info: SocInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.chip.chip_type, "bk72xx");
        assert_eq!(info.flash_baud_rate(), 2_000_000);
        assert_eq!(info.log_baud_rate(), 921_600);
    }
}
