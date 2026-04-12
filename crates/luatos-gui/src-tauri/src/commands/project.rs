//! 项目管理命令

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectInfo {
    pub name: String,
    pub chip: String,
    pub version: String,
    pub description: Option<String>,
    pub script_dirs: Vec<String>,
    pub script_files: Vec<String>,
    pub output_dir: String,
    pub use_luac: bool,
    pub bitw: u32,
    pub luac_debug: bool,
    pub ignore_deps: bool,
    pub soc_file: Option<String>,
    pub port: Option<String>,
    pub baud: Option<u32>,
}

impl From<&luatos_project::Project> for ProjectInfo {
    fn from(p: &luatos_project::Project) -> Self {
        Self {
            name: p.project.name.clone(),
            chip: p.project.chip.clone(),
            version: p.project.version.clone(),
            description: p.project.description.clone(),
            script_dirs: p.build.script_dirs.clone(),
            script_files: p.build.script_files.clone(),
            output_dir: p.build.output_dir.clone(),
            use_luac: p.build.use_luac,
            bitw: p.build.bitw,
            luac_debug: p.build.luac_debug,
            ignore_deps: p.build.ignore_deps,
            soc_file: p.flash.soc_file.clone(),
            port: p.flash.port.clone(),
            baud: p.flash.baud,
        }
    }
}

/// 新建项目
#[tauri::command]
pub fn project_new(dir: String, name: String, chip: String) -> Result<ProjectInfo, String> {
    let dir_path = Path::new(&dir);
    luatos_project::scaffold_project(dir_path, &name, &chip).map_err(|e| format!("新建项目失败: {e}"))?;
    let project = luatos_project::Project::load(dir_path).map_err(|e| format!("加载项目失败: {e}"))?;
    Ok(ProjectInfo::from(&project))
}

/// 打开项目（加载配置）
#[tauri::command]
pub fn project_open(dir: String) -> Result<ProjectInfo, String> {
    let dir_path = Path::new(&dir);
    let project = luatos_project::Project::load(dir_path).map_err(|e| format!("加载项目失败: {e}"))?;
    Ok(ProjectInfo::from(&project))
}

/// 保存项目配置（patch 方式：先加载再修改再保存，避免丢失未知字段）
#[tauri::command]
pub fn project_save(dir: String, info: ProjectInfo) -> Result<ProjectInfo, String> {
    let dir_path = Path::new(&dir);

    // 加载现有项目（如果存在），否则新建
    let mut project = if luatos_project::Project::config_file(dir_path).exists() {
        luatos_project::Project::load(dir_path).map_err(|e| format!("加载项目失败: {e}"))?
    } else {
        luatos_project::Project::new(&info.name, &info.chip)
    };

    // Patch 式更新：只修改前端传来的字段
    project.project.name = info.name;
    project.project.chip = info.chip;
    project.project.version = info.version;
    project.project.description = info.description;
    project.build.script_dirs = info.script_dirs;
    project.build.script_files = info.script_files;
    project.build.output_dir = info.output_dir;
    project.build.use_luac = info.use_luac;
    project.build.bitw = info.bitw;
    project.build.luac_debug = info.luac_debug;
    project.build.ignore_deps = info.ignore_deps;
    project.flash.soc_file = info.soc_file;
    project.flash.port = info.port;
    project.flash.baud = info.baud;

    project.save(dir_path).map_err(|e| format!("保存项目失败: {e}"))?;

    Ok(ProjectInfo::from(&project))
}

/// 导入 LuaTools INI 项目
#[tauri::command]
pub fn project_import(ini_path: String, output_dir: String) -> Result<ProjectInfo, String> {
    let ini = Path::new(&ini_path);
    let out = Path::new(&output_dir);

    let (project, _lt) = luatos_project::import::import_luatools_ini(ini).map_err(|e| format!("导入失败: {e}"))?;

    std::fs::create_dir_all(out).map_err(|e| format!("创建目录失败: {e}"))?;
    project.save(out).map_err(|e| format!("保存项目失败: {e}"))?;

    Ok(ProjectInfo::from(&project))
}
