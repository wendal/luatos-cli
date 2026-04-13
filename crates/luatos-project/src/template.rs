//! LuatOS 项目模板：文件内容生成与占位符替换。
//!
//! 使用 `{{project_name}}` / `{{model_name}}` / `{{chip}}` / `{{version}}`
//! 作为占位符，在生成项目文件时替换为实际值。

use std::path::Path;

use anyhow::{Context, Result};

use crate::wizard::TemplateKind;

/// 模板变量（用于占位符替换）
pub struct TemplateVars<'a> {
    /// 项目名称，如 "my-app"
    pub project_name: &'a str,
    /// 模组型号，如 "Air8101"
    pub model_name: &'a str,
    /// 芯片族，如 "bk72xx"
    pub chip: &'a str,
    /// 项目版本，如 "0.1.0"
    pub version: &'a str,
}

impl<'a> TemplateVars<'a> {
    /// 将字符串中所有占位符替换为实际值
    pub fn render(&self, s: &str) -> String {
        s.replace("{{project_name}}", self.project_name)
            .replace("{{model_name}}", self.model_name)
            .replace("{{chip}}", self.chip)
            .replace("{{version}}", self.version)
    }
}

/// 将模板文件写入指定目录。
///
/// 创建 `lua/` 子目录，写入 `lua/main.lua` 和 `README.md`。
/// 占位符会被 `vars` 中的值替换。
pub fn apply_template(dir: &Path, kind: &TemplateKind, vars: &TemplateVars) -> Result<()> {
    let lua_dir = dir.join("lua");
    std::fs::create_dir_all(&lua_dir).with_context(|| format!("创建 lua 目录失败: {}", lua_dir.display()))?;

    let main_lua_content = match kind {
        TemplateKind::HelloWorld => vars.render(HELLOWORLD_MAIN_LUA),
        TemplateKind::Ui => vars.render(UI_MAIN_LUA),
        TemplateKind::Empty => vars.render(EMPTY_MAIN_LUA),
    };

    let main_lua_path = lua_dir.join("main.lua");
    std::fs::write(&main_lua_path, main_lua_content).with_context(|| format!("写入 main.lua 失败: {}", main_lua_path.display()))?;

    let readme_path = dir.join("README.md");
    std::fs::write(&readme_path, vars.render(README_TEMPLATE)).with_context(|| format!("写入 README.md 失败: {}", readme_path.display()))?;

    log::debug!("模板 {} 已写入 {}", kind.id(), dir.display());
    Ok(())
}

// ─── 模板内容 ──────────────────────────────────────────

const HELLOWORLD_MAIN_LUA: &str = r#"-- LuatOS 示例项目：{{project_name}}
-- 模组: {{model_name}}  芯片: {{chip}}
-- 项目版本: {{version}}
PROJECT = "{{project_name}}"
VERSION = "{{version}}"

log.info("main", PROJECT, VERSION)

-- 核心系统框架（必须）
_G.sys = require("sys")

-- 看门狗：防止程序卡死（可选）
if wdt then
    wdt.init(9000) -- 9s 超时
    sys.timerLoopStart(wdt.feed, 3000) -- 每 3s 喂狗
end

-- 主任务
sys.taskInit(function()
    local counter = 0
    while true do
        counter = counter + 1
        log.info("task", "hello LuatOS", counter)
        log.info("mem", "lua", rtos.meminfo(), "sys", rtos.meminfo("sys"))
        sys.wait(1000)
    end
end)

-- 用户代码结束 -------------------------------------------------
-- 结尾总是这一句，之后不要加任何语句
sys.run()
"#;

const UI_MAIN_LUA: &str = r#"-- LuatOS AirUI 示例：{{project_name}}
-- 模组: {{model_name}}  芯片: {{chip}}
-- 项目版本: {{version}}
PROJECT = "{{project_name}}"
VERSION = "{{version}}"

log.info("main", PROJECT, VERSION)

_G.sys = require("sys")

if wdt then
    wdt.init(9000)
    sys.timerLoopStart(wdt.feed, 3000)
end

sys.taskInit(function()
    -- 初始化 AirUI（480×272 RGB565，根据实际屏幕尺寸调整）
    local ret = airui.init(480, 272, airui.COLOR_FORMAT_RGB565)
    if not ret then
        log.error("airui", "初始化失败，请检查屏幕连接")
        return
    end
    log.info("airui", "初始化成功")

    -- 标签
    local lbl = airui.label({
        parent = airui.screen,
        text = "Hello {{model_name}}!",
        x = 160, y = 80,
        w = 200, h = 40,
    })

    -- 按钮（点击更新标签文字）
    local btn_count = 0
    local btn = airui.button({
        parent = airui.screen,
        text = "点击我",
        x = 180, y = 140, w = 120, h = 48,
        style = "primary",
        on_click = function(self)
            btn_count = btn_count + 1
            lbl:set_text("已点击 " .. btn_count .. " 次")
            log.info("btn", "点击次数", btn_count)
        end,
    })

    -- 开关
    local sw = airui.switch({
        parent = airui.screen,
        checked = false,
        x = 40, y = 200, w = 100, h = 48,
        style = "success",
        on_change = function(self)
            log.info("switch", "状态变化", self:get_state())
        end,
    })

    log.info("airui", "UI 初始化完成")
end)

-- 用户代码结束 -----------------------------------------------
sys.run()
"#;

const EMPTY_MAIN_LUA: &str = r#"-- {{project_name}}
-- 模组: {{model_name}}  芯片: {{chip}}
-- 项目版本: {{version}}
PROJECT = "{{project_name}}"
VERSION = "{{version}}"

_G.sys = require("sys")

-- 在此处编写你的代码

sys.run()
"#;

const README_TEMPLATE: &str = r#"# {{project_name}}

基于 [LuatOS](https://github.com/openLuat/LuatOS) 的 {{model_name}} 项目。

## 硬件

- 模组型号：{{model_name}}
- 芯片：{{chip}}

## 快速开始

```bash
# 下载固件资源（首次使用）
luatos-cli resource download public soc_script

# 构建项目
luatos-cli project build

# 刷写固件（COM6 替换为实际串口）
luatos-cli flash run --soc resource/<型号>/<版本>/<文件>.soc --port COM6
```

## 目录结构

```
{{project_name}}/
├── lua/              # Lua 源码
│   └── main.lua     # 入口文件
├── resource/         # 固件资源（由 luatos-cli resource download 下载，git 忽略）
├── build/            # 构建产物（git 忽略）
└── luatos-project.toml  # 项目配置
```
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wizard::TemplateKind;

    fn test_vars() -> TemplateVars<'static> {
        TemplateVars {
            project_name: "my-app",
            model_name: "Air8101",
            chip: "bk72xx",
            version: "0.1.0",
        }
    }

    #[test]
    fn placeholder_replacement() {
        let vars = test_vars();
        let content = vars.render("项目 {{project_name}} 运行在 {{model_name}} 上，芯片 {{chip}}，版本 {{version}}");
        assert_eq!(content, "项目 my-app 运行在 Air8101 上，芯片 bk72xx，版本 0.1.0");
    }

    #[test]
    fn helloworld_template_structure() {
        let vars = test_vars();
        let content = vars.render(HELLOWORLD_MAIN_LUA);
        assert!(content.contains("sys.run()"), "必须以 sys.run() 结尾");
        assert!(content.contains("my-app"), "项目名应出现在文件中");
        assert!(content.contains("Air8101"), "型号应出现在文件中");
        assert!(content.contains("sys.taskInit"), "应包含主任务");
        assert!(content.contains("_G.sys = require"), "应引入 sys 框架");
        assert!(content.contains("hello LuatOS"), "应有基础输出");
    }

    #[test]
    fn ui_template_structure() {
        let vars = test_vars();
        let content = vars.render(UI_MAIN_LUA);
        assert!(content.contains("airui.init"), "应初始化 AirUI");
        assert!(content.contains("airui.button"), "应有按钮控件");
        assert!(content.contains("airui.label"), "应有标签控件");
        assert!(content.contains("airui.switch"), "应有开关控件");
        assert!(content.contains("sys.run()"), "必须以 sys.run() 结尾");
        assert!(content.contains("Hello Air8101!"), "标签文字应包含型号");
    }

    #[test]
    fn empty_template_minimal() {
        let vars = test_vars();
        let content = vars.render(EMPTY_MAIN_LUA);
        assert!(content.contains("sys.run()"), "必须以 sys.run() 结尾");
        assert!(content.contains("my-app"), "项目名应出现在文件中");
        // 空模板不应有业务逻辑
        assert!(!content.contains("sys.taskInit"), "空模板不应有任务");
    }

    #[test]
    fn apply_template_helloworld_creates_files() {
        let tmp = tempfile::tempdir().unwrap();
        let vars = test_vars();
        apply_template(tmp.path(), &TemplateKind::HelloWorld, &vars).unwrap();

        assert!(tmp.path().join("lua").join("main.lua").exists(), "main.lua 应存在");
        assert!(tmp.path().join("README.md").exists(), "README.md 应存在");

        let content = std::fs::read_to_string(tmp.path().join("lua").join("main.lua")).unwrap();
        assert!(content.contains("my-app"), "main.lua 应包含项目名");
        assert!(content.contains("sys.run()"), "main.lua 应以 sys.run() 结尾");
    }

    #[test]
    fn apply_template_ui_creates_airui_code() {
        let tmp = tempfile::tempdir().unwrap();
        let vars = test_vars();
        apply_template(tmp.path(), &TemplateKind::Ui, &vars).unwrap();

        let content = std::fs::read_to_string(tmp.path().join("lua").join("main.lua")).unwrap();
        assert!(content.contains("airui.init"), "ui 模板应有 airui 初始化");
    }

    #[test]
    fn apply_template_empty_creates_minimal_file() {
        let tmp = tempfile::tempdir().unwrap();
        let vars = test_vars();
        apply_template(tmp.path(), &TemplateKind::Empty, &vars).unwrap();

        let content = std::fs::read_to_string(tmp.path().join("lua").join("main.lua")).unwrap();
        assert!(content.contains("sys.run()"));
        assert!(!content.contains("sys.taskInit"));
    }

    #[test]
    fn readme_contains_model_and_project() {
        let vars = test_vars();
        let content = vars.render(README_TEMPLATE);
        assert!(content.contains("my-app"), "README 应包含项目名");
        assert!(content.contains("Air8101"), "README 应包含型号");
        assert!(content.contains("luatos-cli"), "README 应包含 CLI 命令");
    }
}
