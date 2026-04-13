use std::{env, path::PathBuf, process::Stdio};

use anyhow::Context as _;
use rmcp::{
    handler::server::wrapper::Parameters,
    model::{CallToolResult, Content, LoggingLevel, ProgressNotificationParam},
    service::{RequestContext, RoleServer},
    tool, tool_handler, tool_router,
    transport::stdio,
    ErrorData as McpError, ServerHandler, ServiceExt,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Clone, Default)]
struct LuatosMcp;

#[derive(Debug, Deserialize, JsonSchema)]
struct SerialListArgs {}

#[derive(Debug, Deserialize, JsonSchema)]
struct SocInfoArgs {
    #[schemars(description = "SOC 固件包路径")]
    path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SocFilesArgs {
    #[schemars(description = "SOC 固件包路径")]
    path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SocUnpackArgs {
    #[schemars(description = "SOC 固件包路径")]
    path: String,
    #[schemars(description = "解包输出目录")]
    output: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SocPackArgs {
    #[schemars(description = "待打包目录（必须包含 info.json）")]
    dir: String,
    #[schemars(description = "输出 .soc 文件路径")]
    output: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ProjectInfoArgs {
    #[schemars(description = "项目目录")]
    dir: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ProjectDepsArgs {
    #[schemars(description = "项目目录")]
    dir: Option<String>,
    #[schemars(description = "仅输出可达文件")]
    reachable: Option<bool>,
    #[schemars(description = "仅输出未使用文件")]
    unreachable: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ProjectAnalyzeArgs {
    #[schemars(description = "项目目录")]
    dir: Option<String>,
    #[schemars(description = "可选 SOC 文件路径，用于估算脚本分区容量")]
    soc: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct BuildLuacArgs {
    #[schemars(description = "源码目录列表")]
    src: Vec<String>,
    #[schemars(description = "输出目录")]
    output: String,
    #[schemars(description = "Lua 整数位宽（32 或 64）")]
    bitw: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct BuildFilesystemArgs {
    #[schemars(description = "源码目录列表")]
    src: Vec<String>,
    #[schemars(description = "输出文件路径")]
    output: String,
    #[schemars(description = "是否先编译为 luac")]
    luac: Option<bool>,
    #[schemars(description = "Lua 整数位宽（32 或 64）")]
    bitw: Option<u32>,
    #[schemars(description = "是否附加 BK CRC")]
    bkcrc: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ResourceListArgs {
    #[schemars(description = "可选模组名称，例如 Air8101")]
    module: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ResourceDownloadArgs {
    #[schemars(description = "资源大类，例如 Air8101")]
    category: String,
    #[schemars(description = "子项名或版本号，例如 soc_script / V2032")]
    sub: String,
    #[schemars(description = "可选文件名或版本过滤")]
    item: Option<String>,
    #[schemars(description = "下载输出目录")]
    output: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct FlashRunArgs {
    #[schemars(description = "SOC 固件路径")]
    soc: String,
    #[schemars(description = "串口号，例如 COM6 或 auto")]
    port: String,
    #[schemars(description = "可选波特率")]
    baud: Option<u32>,
    #[schemars(description = "可选脚本目录列表")]
    script: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct FlashScriptArgs {
    #[schemars(description = "SOC 固件路径")]
    soc: String,
    #[schemars(description = "串口号，例如 COM6")]
    port: String,
    #[schemars(description = "脚本目录列表")]
    script: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct FlashTestArgs {
    #[schemars(description = "SOC 固件路径")]
    soc: String,
    #[schemars(description = "串口号，例如 COM6 或 auto")]
    port: String,
    #[schemars(description = "可选波特率")]
    baud: Option<u32>,
    #[schemars(description = "可选脚本目录列表")]
    script: Option<Vec<String>>,
    #[schemars(description = "抓取 boot log 的超时秒数")]
    timeout: Option<u64>,
    #[schemars(description = "要检查的关键字列表")]
    keyword: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct DoctorArgs {
    #[schemars(description = "项目目录")]
    dir: Option<String>,
}

#[derive(Debug, Default)]
struct StreamState {
    final_result: Option<Value>,
    final_error: Option<Value>,
    events: Vec<Value>,
    last_progress: f64,
}

#[tool_router]
impl LuatosMcp {
    // ─── 库直调工具（无需子进程） ─────────────────────────────────────────────

    #[tool(description = "列出当前可用串口")]
    async fn serial_list(&self, _: Parameters<SerialListArgs>, _context: RequestContext<RoleServer>) -> Result<CallToolResult, McpError> {
        let ports = luatos_serial::list_ports();
        lib_result("serial.list", &ports)
    }

    #[tool(description = "读取 SOC 固件包的基础信息")]
    async fn soc_info(&self, Parameters(args): Parameters<SocInfoArgs>, _context: RequestContext<RoleServer>) -> Result<CallToolResult, McpError> {
        let info = luatos_soc::read_soc_info(&args.path).map_err(internal_error)?;
        lib_result("soc.info", &info)
    }

    #[tool(description = "列出 SOC 固件包内的文件列表")]
    async fn soc_files(&self, Parameters(args): Parameters<SocFilesArgs>, _context: RequestContext<RoleServer>) -> Result<CallToolResult, McpError> {
        let files = luatos_soc::list_soc_files(&args.path).map_err(internal_error)?;
        lib_result("soc.files", &files)
    }

    #[tool(description = "解包 SOC 固件到目录")]
    async fn soc_unpack(&self, Parameters(args): Parameters<SocUnpackArgs>, _context: RequestContext<RoleServer>) -> Result<CallToolResult, McpError> {
        let out_dir = args.output.unwrap_or_else(|| {
            std::path::Path::new(&args.path)
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "soc_unpacked".into())
        });
        let unpacked = luatos_soc::unpack_soc(&args.path, std::path::Path::new(&out_dir)).map_err(internal_error)?;
        lib_result(
            "soc.unpack",
            &json!({
                "dir": unpacked.dir.display().to_string(),
                "rom_path": unpacked.rom_path.display().to_string(),
                "flash_exe": unpacked.flash_exe.as_ref().map(|p| p.display().to_string()),
                "chip": unpacked.info.chip.chip_type,
            }),
        )
    }

    #[tool(description = "把目录重新打包为 .soc 固件")]
    async fn soc_pack(&self, Parameters(args): Parameters<SocPackArgs>, _context: RequestContext<RoleServer>) -> Result<CallToolResult, McpError> {
        luatos_soc::pack_soc(std::path::Path::new(&args.dir), &args.output).map_err(internal_error)?;
        lib_result("soc.pack", &json!({"output": args.output}))
    }

    #[tool(description = "读取 LuatOS 项目配置与基础信息")]
    async fn project_info(&self, Parameters(args): Parameters<ProjectInfoArgs>, _context: RequestContext<RoleServer>) -> Result<CallToolResult, McpError> {
        let dir = args.dir.unwrap_or_else(|| ".".into());
        let project = luatos_project::Project::load(std::path::Path::new(&dir)).map_err(internal_error)?;
        lib_result("project.info", &project)
    }

    #[tool(description = "分析 LuatOS 项目依赖图")]
    async fn project_deps(&self, Parameters(args): Parameters<ProjectDepsArgs>, context: RequestContext<RoleServer>) -> Result<CallToolResult, McpError> {
        // 依赖分析涉及较多逻辑（脚本收集+分析+格式化），仍走 CLI 子进程
        let mut cli_args = vec!["project".into(), "deps".into(), "--dir".into(), args.dir.unwrap_or_else(|| ".".into())];
        if args.reachable.unwrap_or(false) {
            cli_args.push("--reachable".into());
        }
        if args.unreachable.unwrap_or(false) {
            cli_args.push("--unreachable".into());
        }
        self.run_tool("project.deps", cli_args, context).await
    }

    #[tool(description = "综合分析项目脚本语法、依赖与分区占用")]
    async fn project_analyze(&self, Parameters(args): Parameters<ProjectAnalyzeArgs>, context: RequestContext<RoleServer>) -> Result<CallToolResult, McpError> {
        let mut cli_args = vec!["project".into(), "analyze".into(), "--dir".into(), args.dir.unwrap_or_else(|| ".".into())];
        push_opt_flag(&mut cli_args, "--soc", args.soc);
        self.run_tool("project.analyze", cli_args, context).await
    }

    #[tool(description = "批量编译 Lua 源码到输出目录")]
    async fn build_luac(&self, Parameters(args): Parameters<BuildLuacArgs>, context: RequestContext<RoleServer>) -> Result<CallToolResult, McpError> {
        let mut cli_args = vec!["build".into(), "luac".into()];
        push_repeat_flag(&mut cli_args, "--src", args.src);
        cli_args.push("--output".into());
        cli_args.push(args.output);
        cli_args.push("--bitw".into());
        cli_args.push(args.bitw.unwrap_or(32).to_string());
        self.run_tool("build.luac", cli_args, context).await
    }

    #[tool(description = "构建 LuaDB 文件系统镜像")]
    async fn build_filesystem(&self, Parameters(args): Parameters<BuildFilesystemArgs>, context: RequestContext<RoleServer>) -> Result<CallToolResult, McpError> {
        let mut cli_args = vec!["build".into(), "filesystem".into()];
        push_repeat_flag(&mut cli_args, "--src", args.src);
        cli_args.push("--output".into());
        cli_args.push(args.output);
        if args.luac.unwrap_or(true) {
            cli_args.push("--luac".into());
        }
        cli_args.push("--bitw".into());
        cli_args.push(args.bitw.unwrap_or(32).to_string());
        if args.bkcrc.unwrap_or(false) {
            cli_args.push("--bkcrc".into());
        }
        self.run_tool("build.filesystem", cli_args, context).await
    }

    #[tool(description = "列出 LuatOS CDN 上的固件资源")]
    async fn resource_list(&self, Parameters(args): Parameters<ResourceListArgs>, _context: RequestContext<RoleServer>) -> Result<CallToolResult, McpError> {
        let manifest = luatos_resource::fetch_manifest().map_err(internal_error)?;
        match args.module {
            Some(ref module) => match luatos_resource::find_category(&manifest, module) {
                Some(category) => lib_result("resource.list", &category),
                None => {
                    let names: Vec<&str> = manifest.resouces.iter().map(|c| c.name.as_str()).collect();
                    Err(McpError::internal_error(format!("未找到模组 '{module}'。可用模组: {}", names.join(", ")), None))
                }
            },
            None => {
                let summary: Vec<Value> = manifest
                    .resouces
                    .iter()
                    .map(|c| {
                        json!({
                            "name": c.name,
                            "desc": c.desc,
                            "children_count": c.childrens.len(),
                        })
                    })
                    .collect();
                lib_result("resource.list", &summary)
            }
        }
    }

    #[tool(description = "诊断开发环境（串口、项目配置、固件、工具链）")]
    async fn doctor(&self, Parameters(args): Parameters<DoctorArgs>, _context: RequestContext<RoleServer>) -> Result<CallToolResult, McpError> {
        // doctor 走 CLI 子进程（它自带格式化输出）
        // 但核心检查也可以在库层面做
        let dir = args.dir.unwrap_or_else(|| ".".into());
        let mut checks = Vec::new();

        // 串口
        let ports = luatos_serial::list_ports();
        checks.push(json!({
            "name": "串口检测",
            "passed": !ports.is_empty(),
            "detail": if ports.is_empty() { "未发现串口".into() } else { format!("发现 {} 个串口", ports.len()) },
            "ports": ports,
        }));

        // 项目配置
        let project_ok = luatos_project::Project::load(std::path::Path::new(&dir)).is_ok();
        checks.push(json!({
            "name": "项目配置",
            "passed": project_ok,
            "detail": if project_ok { "luatos-project.toml 有效" } else { "未找到或无法解析 luatos-project.toml" },
        }));

        // Lua 编译器
        let luac_ok = luatos_luadb::build::compile_lua_bytes(b"print('test')", "test", false, 32).is_ok();
        checks.push(json!({
            "name": "Lua 编译器",
            "passed": luac_ok,
            "detail": if luac_ok { "内嵌 luac 可用" } else { "编译测试失败" },
        }));

        lib_result(
            "doctor",
            &json!({
                "checks": checks,
                "passed": checks.iter().filter(|c| c["passed"].as_bool().unwrap_or(false)).count(),
                "failed": checks.iter().filter(|c| !c["passed"].as_bool().unwrap_or(false)).count(),
            }),
        )
    }

    // ─── 硬件操作仍走子进程 ──────────────────────────────────────────────────

    #[tool(description = "下载 LuatOS 固件/脚本资源")]
    async fn resource_download(&self, Parameters(args): Parameters<ResourceDownloadArgs>, context: RequestContext<RoleServer>) -> Result<CallToolResult, McpError> {
        let mut cli_args = vec!["resource".into(), "download".into(), args.category, args.sub];
        if let Some(item) = args.item {
            cli_args.push(item);
        }
        cli_args.push("--output".into());
        cli_args.push(args.output);
        self.run_tool("resource.download", cli_args, context).await
    }

    #[tool(description = "执行整包刷机")]
    async fn flash_run(&self, Parameters(args): Parameters<FlashRunArgs>, context: RequestContext<RoleServer>) -> Result<CallToolResult, McpError> {
        let mut cli_args = vec!["flash".into(), "run".into(), "--soc".into(), args.soc, "--port".into(), args.port];
        push_opt_flag(&mut cli_args, "--baud", args.baud.map(|v| v.to_string()));
        push_repeat_flag(&mut cli_args, "--script", args.script.unwrap_or_default());
        self.run_tool("flash.run", cli_args, context).await
    }

    #[tool(description = "仅刷写脚本区")]
    async fn flash_script(&self, Parameters(args): Parameters<FlashScriptArgs>, context: RequestContext<RoleServer>) -> Result<CallToolResult, McpError> {
        let mut cli_args = vec!["flash".into(), "script".into(), "--soc".into(), args.soc, "--port".into(), args.port];
        push_repeat_flag(&mut cli_args, "--script", args.script);
        self.run_tool("flash.script", cli_args, context).await
    }

    #[tool(description = "执行闭环刷机测试并检查启动关键字")]
    async fn flash_test(&self, Parameters(args): Parameters<FlashTestArgs>, context: RequestContext<RoleServer>) -> Result<CallToolResult, McpError> {
        let mut cli_args = vec!["flash".into(), "test".into(), "--soc".into(), args.soc, "--port".into(), args.port];
        push_opt_flag(&mut cli_args, "--baud", args.baud.map(|v| v.to_string()));
        push_repeat_flag(&mut cli_args, "--script", args.script.unwrap_or_default());
        cli_args.push("--timeout".into());
        cli_args.push(args.timeout.unwrap_or(15).to_string());
        push_repeat_flag(&mut cli_args, "--keyword", args.keyword.unwrap_or_else(|| vec!["LuatOS@".into()]));
        self.run_tool("flash.test", cli_args, context).await
    }
}

#[tool_handler(
    name = "luatos-mcp",
    version = "1.7.0",
    instructions = "LuatOS MCP server — 串口/SOC/项目/资源等查询操作走库直调（零延迟），刷机/日志等硬件操作走 CLI 子进程。新增 doctor 工具用于环境诊断。"
)]
impl ServerHandler for LuatosMcp {}

impl LuatosMcp {
    async fn run_tool(&self, command_name: &str, cli_args: Vec<String>, context: RequestContext<RoleServer>) -> Result<CallToolResult, McpError> {
        let cli_bin = resolve_cli_binary().map_err(internal_error)?;
        let mut child = Command::new(&cli_bin);
        child
            .arg("--format")
            .arg("jsonl")
            .args(&cli_args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = child
            .spawn()
            .map_err(|e| internal_error(anyhow::anyhow!("启动 luatos-cli 失败: {} ({e})", cli_bin.display())))?;

        let stdout = child.stdout.take().ok_or_else(|| internal_error(anyhow::anyhow!("无法获取 luatos-cli stdout")))?;
        let stderr = child.stderr.take().ok_or_else(|| internal_error(anyhow::anyhow!("无法获取 luatos-cli stderr")))?;

        let stderr_task = tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            let mut collected = Vec::new();
            while let Ok(Some(line)) = lines.next_line().await {
                if !line.trim().is_empty() {
                    collected.push(line);
                }
            }
            collected
        });

        let progress_token = context.meta.get_progress_token();
        let mut stdout_lines = BufReader::new(stdout).lines();
        let mut state = StreamState::default();

        loop {
            tokio::select! {
                _ = context.ct.cancelled() => {
                    let _ = child.kill().await;
                    return Err(McpError::internal_error("工具调用已取消", None));
                }
                line = stdout_lines.next_line() => {
                    let line = line.map_err(|e| internal_error(e.into()))?;
                    let Some(line) = line else {
                        break;
                    };
                    if line.trim().is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<Value>(&line) {
                        Ok(value) => handle_event(command_name, value, &mut state, &progress_token, &context).await?,
                        Err(err) => {
                            state.events.push(json!({
                                "type": "unparsed_stdout",
                                "command": command_name,
                                "line": line,
                                "error": err.to_string(),
                            }));
                        }
                    }
                }
            }
        }

        let status = child.wait().await.map_err(|e| internal_error(e.into()))?;
        let stderr_lines = stderr_task.await.map_err(|e| internal_error(anyhow::anyhow!("读取 stderr 失败: {e}")))?;

        if !stderr_lines.is_empty() {
            let _ = context
                .peer
                .notify_logging_message(rmcp::model::LoggingMessageNotificationParam {
                    level: LoggingLevel::Info,
                    logger: Some("luatos-cli".into()),
                    data: json!(stderr_lines),
                })
                .await;
        }

        let payload = finalize_payload(command_name, status.code(), state.final_result, state.final_error, state.events, stderr_lines);
        let summary = summarize_payload(&payload);

        let mut result = if status.success() && payload["status"].as_str() == Some("ok") {
            CallToolResult::structured(payload)
        } else {
            CallToolResult::structured_error(payload)
        };
        result.content.push(Content::text(summary));
        Ok(result)
    }
}

async fn handle_event(
    command_name: &str,
    value: Value,
    state: &mut StreamState,
    progress_token: &Option<rmcp::model::ProgressToken>,
    context: &RequestContext<RoleServer>,
) -> Result<(), McpError> {
    match value.get("type").and_then(Value::as_str) {
        Some("result") => {
            state.final_result = Some(value);
        }
        Some("error") => {
            state.final_error = Some(value);
        }
        Some("progress") => {
            if let Some(token) = progress_token.clone() {
                if let Some(percent) = value.get("percent").and_then(Value::as_f64) {
                    let progress = percent.max(state.last_progress);
                    state.last_progress = progress;
                    let message = value.get("message").and_then(Value::as_str).map(ToOwned::to_owned);
                    let _ = context
                        .peer
                        .notify_progress(
                            ProgressNotificationParam::new(token, progress)
                                .with_total(100.0)
                                .with_message(message.unwrap_or_else(|| format!("{command_name} 进行中"))),
                        )
                        .await;
                }
            }
        }
        Some("message") | Some("log_entry") | Some("resource_download") | Some("boot_log_line") | Some("unparsed_stdout") => {
            state.events.push(value);
        }
        Some(_) | None => {
            state.events.push(value);
        }
    }
    Ok(())
}

fn finalize_payload(command_name: &str, exit_code: Option<i32>, final_result: Option<Value>, final_error: Option<Value>, events: Vec<Value>, stderr: Vec<String>) -> Value {
    if let Some(mut result) = final_result {
        if let Some(obj) = result.as_object_mut() {
            if !events.is_empty() {
                obj.insert("events".into(), Value::Array(events));
            }
            if !stderr.is_empty() {
                obj.insert("stderr".into(), json!(stderr));
            }
            obj.insert("exit_code".into(), json!(exit_code));
        }
        return result;
    }

    if let Some(mut error) = final_error {
        if let Some(obj) = error.as_object_mut() {
            obj.entry("status").or_insert_with(|| json!("error"));
            obj.entry("command").or_insert_with(|| json!(command_name));
            if !events.is_empty() {
                obj.insert("events".into(), Value::Array(events));
            }
            if !stderr.is_empty() {
                obj.insert("stderr".into(), json!(stderr));
            }
            obj.insert("exit_code".into(), json!(exit_code));
        }
        return error;
    }

    json!({
        "status": if exit_code == Some(0) { "ok" } else { "error" },
        "command": command_name,
        "data": Value::Null,
        "events": events,
        "stderr": stderr,
        "exit_code": exit_code,
    })
}

fn summarize_payload(payload: &Value) -> String {
    let command = payload.get("command").and_then(Value::as_str).unwrap_or("luatos-cli");
    let status = payload.get("status").and_then(Value::as_str).unwrap_or("unknown");
    match payload.get("data") {
        Some(data) if !data.is_null() => format!("{command}: {status}\n{}", serde_json::to_string_pretty(data).unwrap_or_default()),
        _ => format!("{command}: {status}"),
    }
}

fn resolve_cli_binary() -> anyhow::Result<PathBuf> {
    if let Ok(path) = env::var("LUATOS_CLI_BIN") {
        return Ok(PathBuf::from(path));
    }

    let current = env::current_exe().context("读取当前 luatos-mcp 路径失败")?;
    if let Some(dir) = current.parent() {
        let sibling = dir.join(format!("luatos-cli{}", env::consts::EXE_SUFFIX));
        if sibling.exists() {
            return Ok(sibling);
        }
    }

    Ok(PathBuf::from(format!("luatos-cli{}", env::consts::EXE_SUFFIX)))
}

fn push_opt_flag(args: &mut Vec<String>, flag: &str, value: Option<String>) {
    if let Some(value) = value {
        args.push(flag.into());
        args.push(value);
    }
}

fn push_repeat_flag(args: &mut Vec<String>, flag: &str, values: Vec<String>) {
    for value in values {
        args.push(flag.into());
        args.push(value);
    }
}

fn internal_error(error: anyhow::Error) -> McpError {
    McpError::internal_error(error.to_string(), None)
}

/// 把库直调的结果包装成 MCP CallToolResult（JSON 格式）
fn lib_result<T: serde::Serialize>(command: &str, data: &T) -> Result<CallToolResult, McpError> {
    let envelope = json!({
        "status": "ok",
        "command": command,
        "data": data,
    });
    let text = serde_json::to_string_pretty(&envelope).map_err(|e| McpError::internal_error(e.to_string(), None))?;
    Ok(CallToolResult::success(vec![Content::text(text)]))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive(tracing::Level::INFO.into()))
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let service = LuatosMcp.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finalize_payload_preserves_events() {
        let payload = finalize_payload(
            "flash.run",
            Some(0),
            Some(json!({
                "type": "result",
                "command": "flash.run",
                "status": "ok",
                "data": { "chip": "air8101" }
            })),
            None,
            vec![json!({"type":"message","message":"hello"})],
            vec![],
        );

        assert_eq!(payload["command"], "flash.run");
        assert_eq!(payload["events"][0]["message"], "hello");
    }

    #[test]
    fn summarize_payload_prefers_data() {
        let summary = summarize_payload(&json!({
            "command": "serial.list",
            "status": "ok",
            "data": [{ "port_name": "COM6" }]
        }));

        assert!(summary.contains("serial.list: ok"));
        assert!(summary.contains("COM6"));
    }
}
