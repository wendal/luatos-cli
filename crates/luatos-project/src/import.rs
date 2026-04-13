//! Import LuaTools INI project files.
//!
//! LuaTools stores project configurations as `.ini` files with this structure:
//!
//! ```ini
//! [info]
//! core_path = D:\path\to\firmware.soc
//! type = .soc
//! active = False
//! luac_debug = False
//! ...
//!
//! [D:\path\to\scripts\dir1]
//! main.lua =
//! utils.lua =
//!
//! [D:\path\to\scripts\dir2]
//! lib.lua =
//! ```
//!
//! The `[info]` section contains project metadata, while additional sections
//! represent script directories where the keys are filenames.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::{default_bitw, BuildConfig, FlashConfig, Project, ProjectMeta};

/// Parsed representation of a LuaTools `.ini` project file.
#[derive(Debug, Clone)]
pub struct LuatoolsProject {
    /// Values from the `[info]` section.
    pub info: BTreeMap<String, String>,
    /// Script directories: each key is a directory path, value is a list of filenames.
    pub script_sections: Vec<ScriptSection>,
}

/// A script directory section from the INI file.
#[derive(Debug, Clone)]
pub struct ScriptSection {
    /// Absolute directory path (as it appeared in the INI header).
    pub dir_path: String,
    /// File names within this directory.
    pub files: Vec<String>,
}

/// Parse a LuaTools `.ini` file into a [`LuatoolsProject`].
pub fn parse_luatools_ini(content: &str) -> Result<LuatoolsProject> {
    let mut info = BTreeMap::new();
    let mut script_sections = Vec::new();
    let mut current_section: Option<String> = None;
    let mut current_files: Vec<String> = Vec::new();

    for line in content.lines() {
        let line = line.trim();

        // Skip empty lines
        if line.is_empty() {
            continue;
        }

        // Section header
        if line.starts_with('[') && line.ends_with(']') {
            // Flush previous script section
            if let Some(ref section_name) = current_section {
                if section_name != "info" {
                    script_sections.push(ScriptSection {
                        dir_path: section_name.clone(),
                        files: std::mem::take(&mut current_files),
                    });
                }
            }

            let section_name = line[1..line.len() - 1].to_string();
            current_section = Some(section_name);
            current_files.clear();
            continue;
        }

        // Key = value pair
        if let Some(eq_pos) = line.find('=') {
            let key = line[..eq_pos].trim().to_string();
            let value = line[eq_pos + 1..].trim().to_string();

            match current_section.as_deref() {
                Some("info") => {
                    info.insert(key, value);
                }
                Some(_) => {
                    // In a script section: key is the filename, value is usually empty
                    if !key.is_empty() {
                        current_files.push(key);
                    }
                }
                None => {
                    // Before any section — ignore
                }
            }
        }
    }

    // Flush last section
    if let Some(ref section_name) = current_section {
        if section_name != "info" && !current_files.is_empty() {
            script_sections.push(ScriptSection {
                dir_path: section_name.clone(),
                files: current_files,
            });
        }
    }

    Ok(LuatoolsProject { info, script_sections })
}

/// Detect chip identifier from a SOC filename.
///
/// Examples:
/// - `LuatOS-SoC_V2001_Air8101.soc` → `"bk72xx"`
/// - `LuatOS-SoC_V2004_Air780EPM.soc` → `"ec7xx"`
/// - `LuatOS-SoC_V2031_Air8000_101.soc` → `"air8000"`
fn detect_chip_from_soc_path(soc_path: &str) -> String {
    let lower = soc_path.to_lowercase();
    let filename = Path::new(&lower).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| lower.clone());

    if filename.contains("air8101") || filename.contains("bk72") || filename.contains("bk7258") {
        "bk72xx".to_string()
    } else if filename.contains("air6208") {
        "air6208".to_string()
    } else if filename.contains("air101") && !filename.contains("air1016") {
        "air101".to_string()
    } else if filename.contains("air8000") {
        "air8000".to_string()
    } else if filename.contains("air780")
        || filename.contains("air600")
        || filename.contains("ec718")
        || filename.contains("ec618")
        || filename.contains("air1601")
        || filename.contains("air201")
    {
        "ec7xx".to_string()
    } else if filename.contains("esp32") {
        "esp32".to_string()
    } else {
        "unknown".to_string()
    }
}

/// Derive a project name from the INI filename (without extension).
fn project_name_from_path(ini_path: &Path) -> String {
    ini_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "imported_project".to_string())
}

/// Import a LuaTools `.ini` project file and convert it to a [`Project`].
///
/// The resulting project will reference the original script directories.
/// `project_dir` is the target directory where `luatos-project.toml` will be saved.
pub fn import_luatools_ini(ini_path: &Path) -> Result<(Project, LuatoolsProject)> {
    let content = fs::read_to_string(ini_path).with_context(|| format!("failed to read {}", ini_path.display()))?;

    import_luatools_ini_str(&content, ini_path)
}

/// Import from an INI string, with the file path used for naming.
pub fn import_luatools_ini_str(content: &str, ini_path: &Path) -> Result<(Project, LuatoolsProject)> {
    let lt_project = parse_luatools_ini(content)?;

    // Extract SOC path
    let soc_path = lt_project.info.get("core_path").cloned();
    let chip = soc_path.as_deref().map(detect_chip_from_soc_path).unwrap_or_else(|| "unknown".to_string());

    let name = project_name_from_path(ini_path);

    // Collect script directories
    let script_dirs: Vec<String> = lt_project.script_sections.iter().map(|s| s.dir_path.clone()).collect();

    if script_dirs.is_empty() {
        bail!("No script sections found in INI file");
    }

    // Check luac_debug setting
    let luac_debug = lt_project.info.get("luac_debug").map(|v| v.eq_ignore_ascii_case("true")).unwrap_or(false);

    let project = Project {
        project: ProjectMeta {
            name,
            chip: chip.clone(),
            version: "0.1.0".to_string(),
            description: Some("Imported from LuaTools".to_string()),
        },
        build: BuildConfig {
            script_dirs,
            script_files: Vec::new(),
            output_dir: lt_project
                .info
                .get("output_path")
                .filter(|s| !s.is_empty())
                .cloned()
                .unwrap_or_else(|| "build/".to_string()),
            use_luac: !lt_project.info.get("only_luac_code").map(|v| v.eq_ignore_ascii_case("true")).unwrap_or(false),
            bitw: default_bitw(&chip),
            luac_debug,
            ignore_deps: false,
            soc_script: "latest".to_string(),
            resource_dir: "resource/".to_string(),
        },
        flash: FlashConfig {
            soc_file: soc_path,
            port: None,
            baud: None,
        },
    };

    Ok((project, lt_project))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_INI: &str = r#"[info]
core_path = D:\github\luatos-soc-2024\project\luatos\out\LuatOS-SoC_V2001_Air8101.soc
type = .soc
active = False
lib =
demo =
output_path =
output_suffix_enable = False
output_suffix =
code_enable = False
code =
print_mode = 2
add_core = False
only_code = False
only_luac_code = False

[D:\github\LuatOS\demo\gpio\gpio]
main.lua =
readme.md =

[D:\github\LuatOS\demo\common\libs]
sys.lua =
"#;

    #[test]
    fn parse_ini_structure() {
        let project = parse_luatools_ini(SAMPLE_INI).unwrap();

        assert_eq!(
            project.info.get("core_path").unwrap(),
            r"D:\github\luatos-soc-2024\project\luatos\out\LuatOS-SoC_V2001_Air8101.soc"
        );
        assert_eq!(project.info.get("type").unwrap(), ".soc");
        assert_eq!(project.script_sections.len(), 2);
        assert_eq!(project.script_sections[0].dir_path, r"D:\github\LuatOS\demo\gpio\gpio");
        assert_eq!(project.script_sections[0].files, vec!["main.lua", "readme.md"]);
        assert_eq!(project.script_sections[1].dir_path, r"D:\github\LuatOS\demo\common\libs");
        assert_eq!(project.script_sections[1].files, vec!["sys.lua"]);
    }

    #[test]
    fn import_ini_to_project() {
        let ini_path = Path::new("test_project.ini");
        let (project, _) = import_luatools_ini_str(SAMPLE_INI, ini_path).unwrap();

        assert_eq!(project.project.name, "test_project");
        assert_eq!(project.project.chip, "bk72xx");
        assert_eq!(project.build.script_dirs.len(), 2);
        assert_eq!(project.build.bitw, 32);
        assert!(!project.build.luac_debug);
        assert!(project.flash.soc_file.is_some());
    }

    #[test]
    fn detect_chip_variants() {
        assert_eq!(detect_chip_from_soc_path("LuatOS-SoC_V2001_Air8101.soc"), "bk72xx");
        assert_eq!(detect_chip_from_soc_path("LuatOS-SoC_V2004_Air780EPM.soc"), "ec7xx");
        assert_eq!(detect_chip_from_soc_path("LuatOS-SoC_V2031_Air8000_101.soc"), "air8000");
        assert_eq!(detect_chip_from_soc_path("LuatOS-SoC_V1001_Air6208.soc"), "air6208");
        assert_eq!(detect_chip_from_soc_path("D:\\path\\to\\LuatOS-SoC_Air101.soc"), "air101");
    }

    #[test]
    fn import_ini_with_luac_debug() {
        let ini_with_debug = r#"[info]
core_path = LuatOS-SoC_V2001_Air8101.soc
type = .soc
luac_debug = True

[D:\scripts]
main.lua =
"#;
        let ini_path = Path::new("debug_project.ini");
        let (project, _) = import_luatools_ini_str(ini_with_debug, ini_path).unwrap();
        assert!(project.build.luac_debug);
    }

    #[test]
    fn import_ini_no_scripts_fails() {
        let ini_no_scripts = r#"[info]
core_path = test.soc
type = .soc
"#;
        let ini_path = Path::new("empty.ini");
        let result = import_luatools_ini_str(ini_no_scripts, ini_path);
        assert!(result.is_err());
    }

    #[test]
    fn parse_ini_with_empty_values() {
        let ini = r#"[info]
core_path = test.soc
type = .soc
lib =
output_path =

[D:\test]
main.lua =
"#;
        let project = parse_luatools_ini(ini).unwrap();
        assert_eq!(project.info.get("lib").unwrap(), "");
        assert_eq!(project.info.get("output_path").unwrap(), "");
    }
}
