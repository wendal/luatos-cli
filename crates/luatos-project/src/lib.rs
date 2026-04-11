//! LuatOS project configuration management.
//!
//! This crate handles reading, writing, and scaffolding LuatOS project
//! configuration files (`luatos-project.toml`). It supports chip-specific
//! defaults and provides a convenient project initialization workflow.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// Name of the project configuration file.
pub const CONFIG_FILE_NAME: &str = "luatos-project.toml";

/// Default script source directory.
const DEFAULT_SCRIPT_DIR: &str = "lua/";

/// Default build output directory.
const DEFAULT_OUTPUT_DIR: &str = "build/";

/// Returns the default Lua integer bit-width for a given chip.
///
/// Chips that use a 64-bit Lua VM: `air6208`, `air101`.
/// All other chips default to 32-bit.
///
/// # Examples
///
/// ```
/// assert_eq!(luatos_project::default_bitw("air6208"), 64);
/// assert_eq!(luatos_project::default_bitw("bk72xx"), 32);
/// ```
pub fn default_bitw(chip: &str) -> u32 {
    match chip {
        "air6208" | "air101" => 64,
        _ => 32,
    }
}

/// Top-level project configuration, corresponding to `luatos-project.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Project {
    /// General project metadata.
    pub project: ProjectMeta,
    /// Build-related settings.
    pub build: BuildConfig,
    /// Flashing / download settings.
    pub flash: FlashConfig,
}

/// General project metadata.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProjectMeta {
    /// Human-readable project name.
    pub name: String,
    /// Target chip identifier (e.g. `"bk72xx"`, `"air6208"`).
    pub chip: String,
    /// Semantic version string (e.g. `"0.1.0"`).
    pub version: String,
    /// Optional one-line description.
    pub description: Option<String>,
}

/// Build configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BuildConfig {
    /// Directory containing Lua source scripts.
    pub script_dir: String,
    /// Directory where build artifacts are written.
    pub output_dir: String,
    /// Whether to compile scripts with `luac` before packaging.
    pub use_luac: bool,
    /// Lua integer bit-width (`32` or `64`), typically determined by chip.
    pub bitw: u32,
}

/// Flash / download configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FlashConfig {
    /// Path to the `.soc` firmware descriptor file.
    pub soc_file: Option<String>,
    /// Serial port name (e.g. `"COM6"`, `"/dev/ttyUSB0"`).
    pub port: Option<String>,
    /// Override baud rate for the serial connection.
    pub baud: Option<u32>,
}

impl Project {
    /// Create a new project configuration with sensible defaults for the
    /// given `chip`.
    ///
    /// The Lua bit-width is derived automatically via [`default_bitw`].
    pub fn new(name: &str, chip: &str) -> Self {
        Self {
            project: ProjectMeta {
                name: name.to_string(),
                chip: chip.to_string(),
                version: "0.1.0".to_string(),
                description: None,
            },
            build: BuildConfig {
                script_dir: DEFAULT_SCRIPT_DIR.to_string(),
                output_dir: DEFAULT_OUTPUT_DIR.to_string(),
                use_luac: true,
                bitw: default_bitw(chip),
            },
            flash: FlashConfig {
                soc_file: None,
                port: None,
                baud: None,
            },
        }
    }

    /// Return the path to `luatos-project.toml` inside `dir`.
    pub fn config_file(dir: &Path) -> PathBuf {
        dir.join(CONFIG_FILE_NAME)
    }

    /// Load a project configuration from `{dir}/luatos-project.toml`.
    pub fn load(dir: &Path) -> Result<Self> {
        let path = Self::config_file(dir);
        let content = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let project: Project = toml::from_str(&content)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        Ok(project)
    }

    /// Serialize and write the configuration to `{dir}/luatos-project.toml`.
    ///
    /// Parent directories are **not** created automatically; `dir` must
    /// already exist.
    pub fn save(&self, dir: &Path) -> Result<()> {
        let path = Self::config_file(dir);
        let content =
            toml::to_string_pretty(self).context("failed to serialize project config")?;
        fs::write(&path, content)
            .with_context(|| format!("failed to write {}", path.display()))?;
        Ok(())
    }
}

/// Scaffold a new LuatOS project inside `dir`.
///
/// This creates:
/// - `luatos-project.toml` with defaults for the given chip
/// - `lua/` directory
/// - `lua/main.lua` with a hello-world template
///
/// Returns an error if `luatos-project.toml` already exists in `dir`.
pub fn scaffold_project(dir: &Path, name: &str, chip: &str) -> Result<()> {
    let config_path = Project::config_file(dir);
    if config_path.exists() {
        bail!(
            "{} already exists in {}",
            CONFIG_FILE_NAME,
            dir.display()
        );
    }

    // Ensure the target directory exists.
    fs::create_dir_all(dir)
        .with_context(|| format!("failed to create directory {}", dir.display()))?;

    // Write project config.
    let project = Project::new(name, chip);
    project.save(dir)?;

    // Create lua/ directory and main.lua template.
    let lua_dir = dir.join("lua");
    fs::create_dir_all(&lua_dir)
        .with_context(|| format!("failed to create {}", lua_dir.display()))?;

    let main_lua = lua_dir.join("main.lua");
    fs::write(
        &main_lua,
        "print(\"Hello from \" .. _VERSION)\n",
    )
    .with_context(|| format!("failed to write {}", main_lua.display()))?;

    log::info!("scaffolded project '{}' for chip '{}' in {}", name, chip, dir.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Round-trip: create → save → load and verify equality.
    #[test]
    fn save_load_round_trip() {
        let tmp = std::env::temp_dir().join("luatos_project_test_round_trip");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let original = Project::new("demo", "air6208");
        original.save(&tmp).unwrap();

        let loaded = Project::load(&tmp).unwrap();
        assert_eq!(original, loaded);

        // Cleanup
        let _ = fs::remove_dir_all(&tmp);
    }

    /// Verify default bit-widths for known chip families.
    #[test]
    fn default_bitw_chips() {
        // 32-bit chips
        assert_eq!(default_bitw("bk72xx"), 32);
        assert_eq!(default_bitw("air8101"), 32);
        assert_eq!(default_bitw("air8000"), 32);

        // 64-bit chips
        assert_eq!(default_bitw("air6208"), 64);
        assert_eq!(default_bitw("air101"), 64);

        // Unknown chip falls back to 32
        assert_eq!(default_bitw("unknown_chip"), 32);
    }

    /// Scaffold creates the expected file tree.
    #[test]
    fn scaffold_creates_files() {
        let tmp = std::env::temp_dir().join("luatos_project_test_scaffold");
        let _ = fs::remove_dir_all(&tmp);

        scaffold_project(&tmp, "hello", "bk72xx").unwrap();

        // Config file exists and is valid.
        let project = Project::load(&tmp).unwrap();
        assert_eq!(project.project.name, "hello");
        assert_eq!(project.project.chip, "bk72xx");
        assert_eq!(project.build.bitw, 32);

        // lua/main.lua exists with expected content.
        let main_lua = fs::read_to_string(tmp.join("lua").join("main.lua")).unwrap();
        assert!(main_lua.contains("Hello from"));
        assert!(main_lua.contains("_VERSION"));

        // Scaffolding again should fail.
        let err = scaffold_project(&tmp, "hello", "bk72xx");
        assert!(err.is_err());

        // Cleanup
        let _ = fs::remove_dir_all(&tmp);
    }

    /// `Project::new` applies chip-specific defaults correctly.
    #[test]
    fn new_project_defaults() {
        let p = Project::new("test_proj", "air6208");
        assert_eq!(p.project.version, "0.1.0");
        assert_eq!(p.build.script_dir, "lua/");
        assert_eq!(p.build.output_dir, "build/");
        assert!(p.build.use_luac);
        assert_eq!(p.build.bitw, 64);
        assert!(p.flash.port.is_none());
    }
}
