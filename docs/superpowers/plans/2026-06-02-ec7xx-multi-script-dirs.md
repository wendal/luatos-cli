# EC7xx 多脚本目录支持实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为 EC7xx/Air8000/Air780EPM 系列芯片添加多脚本目录支持，使得：
1. 全量刷机时可以使用多脚本目录构建脚本镜像
2. 仅刷脚本区时可以使用多脚本目录

**Architecture:**
- 参考 `bk7258.rs` 和 `sf32lb5x.rs` 的已有实现
- 修改 `flash_ec718` 签名，添加 `script_folders: Option<&[&str]>`
- 当提供 `script_folders` 时，调用 `luatos_luadb::build::build_script_image()` 构建脚本镜像并替换 binpkg 中的 script 条目
- GUI (`FlashView.vue`) 支持多目录 UI

**Tech Stack:** Rust, Vue 3, TypeScript, `luatos-luadb`, `luatos-soc`

---

## 文件地图

- `crates/luatos-flash/src/ec718.rs`
  - 修改 `flash_ec718` 签名，添加 `script_folders` 参数处理逻辑
- `crates/luatos-gui/src-tauri/src/commands/flash.rs`
  - 传递 `script_folders` 给 ec718
- `crates/luatos-cli/src/cmd_flash.rs`
  - `cmd_flash_run`: 传递 `script_folders` 给 ec718
  - `cmd_flash_script`: 无需改动（已支持多目录）
- `crates/luatos-gui/src/views/FlashView.vue`
  - 多目录 UI（列表显示、添加、删除）

---

## 参数说明

```rust
// build_script_image 签名
pub fn build_script_image(
    script_dirs: &[&Path],
    use_luac: bool,      // 固定为 true
    bitw: u32,           // 从 SOC info.json 获取
    use_bkcrc: bool,     // 从 SOC info.json 获取
    strip: bool,          // 固定为 true
) -> Result<Vec<u8>>
```

---

## Task 1: 修改 ec718.rs 添加多目录支持

**Files:**
- Modify: `crates/luatos-flash/src/ec718.rs`

- [ ] **Step 1: 修改 `flash_ec718` 函数签名**

在 `crates/luatos-flash/src/ec718.rs` 第 1293 行附近，将：

```rust
pub fn flash_ec718(
    soc_path: &str,
    port: &str,
    on_progress: ProgressCallback,
    cancel: Arc<AtomicBool>,
) -> Result<()>
```

改为：

```rust
pub fn flash_ec718(
    soc_path: &str,
    port: &str,
    script_folders: Option<&[&str]>,
    on_progress: ProgressCallback,
    cancel: Arc<AtomicBool>,
) -> Result<()>
```

- [ ] **Step 2: 在解析 SOC 后添加脚本目录处理逻辑**

在 `flash_ec718` 函数中，解析 SOC 后（约第 1297 行），添加：

```rust
// 解析 SOC
let (mut binpkg, _, force_br) = extract_and_parse_soc(soc_path)?;

// 如果提供了 script_folders，构建新的脚本镜像并替换 binpkg 中的 script 条目
if let Some(folders) = script_folders {
    let dir_paths: Vec<std::path::PathBuf> = folders.iter().map(std::path::PathBuf::from).collect();
    let path_refs: Vec<&std::path::Path> = dir_paths.iter().map(|p| p.as_path()).collect();

    let info = luatos_soc::read_soc_info(soc_path)?;
    let script_data = luatos_luadb::build::build_script_image(
        &path_refs,
        true,                           // use_luac 固定为 true
        info.script_bitw(),              // 从 SOC 获取
        info.use_bkcrc(),               // 从 SOC 获取
        true,                           // strip 固定为 true
    )?;

    // 替换或添加 binpkg 中名为 "script" 的条目
    if let Some(existing) = binpkg.entries.iter_mut().find(|e| e.name == "script") {
        existing.data = Some(script_data);
    } else {
        binpkg.entries.push(BinpkgEntry {
            name: "script".to_string(),
            addr: info.script_addr(),
            flash_size: 0,
            image_size: script_data.len() as u32,
            hash: String::new(),
            image_type: "AP".to_string(),
            data: Some(script_data),
        });
    }
}
```

- [ ] **Step 3: 运行测试验证**

```powershell
cargo test -p luatos-flash
cargo clippy -p luatos-flash -- -D warnings
```

---

## Task 2: 修改 GUI 命令层 (flash.rs)

**Files:**
- Modify: `crates/luatos-gui/src-tauri/src/commands/flash.rs`

- [ ] **Step 1: 修改 ec718 分支调用**

在 `crates/luatos-gui/src-tauri/src/commands/flash.rs` 第 49-51 行，将：

```rust
"ec7xx" | "air8000" | "air780epm" | "air780ehm" | "air780ehv" | "air780ehg" => {
    let boot_port = luatos_flash::ec718::auto_enter_boot_mode(Some(&port), &on_progress).map_err(|e| format!("EC718 进入下载模式失败: {e}"))?;
    luatos_flash::ec718::flash_ec718(&soc_path, &boot_port, &on_progress, cancel).map_err(|e| format!("EC718 刷机失败: {e}"))?;
}
```

改为：

```rust
"ec7xx" | "air8000" | "air780epm" | "air780ehm" | "air780ehv" | "air780ehg" => {
    let folders_refs: Option<Vec<&str>> = script_folders.as_ref().map(|dirs| dirs.iter().map(|s| s.as_str()).collect());
    let boot_port = luatos_flash::ec718::auto_enter_boot_mode(Some(&port), &on_progress).map_err(|e| format!("EC718 进入下载模式失败: {e}"))?;
    luatos_flash::ec718::flash_ec718(&soc_path, &boot_port, folders_refs.as_deref(), &on_progress, cancel).map_err(|e| format!("EC718 刷机失败: {e}"))?;
}
```

- [ ] **Step 2: 验证编译**

```powershell
cargo build -p luatos-gui
```

---

## Task 3: 修改 CLI 命令层 (cmd_flash.rs)

**Files:**
- Modify: `crates/luatos-cli/src/cmd_flash.rs`

- [ ] **Step 1: 修改 `cmd_flash_run` 函数中 ec718 分支**

在 `crates/luatos-cli/src/cmd_flash.rs` 第 184-193 行，将：

```rust
"ec7xx" | "air8000" | "air780epm" | "air780ehm" | "air780ehv" | "air780ehg" => {
    // EC718 series: auto-detect boot mode, reboot if needed
    let boot_port = luatos_flash::ec718::auto_enter_boot_mode(Some(port), &on_progress)?;
    luatos_flash::ec718::flash_ec718(soc, &boot_port, &on_progress, cancel)?;
    match format {
        OutputFormat::Text => {
            println!("EC718 flash completed successfully.");
        }
        OutputFormat::Json | OutputFormat::Jsonl => event::emit_result(format, "flash.run", "ok", serde_json::json!({ "chip": chip }))?,
    }
}
```

改为：

```rust
"ec7xx" | "air8000" | "air780epm" | "air780ehm" | "air780ehv" | "air780ehg" => {
    // EC718 series: auto-detect boot mode, reboot if needed
    let folders_refs: Option<Vec<&str>> = script_folders.map(|dirs| dirs.iter().map(|s| s.as_str()).collect());
    let boot_port = luatos_flash::ec718::auto_enter_boot_mode(Some(port), &on_progress)?;
    luatos_flash::ec718::flash_ec718(soc, &boot_port, folders_refs.as_deref(), &on_progress, cancel)?;
    match format {
        OutputFormat::Text => {
            println!("EC718 flash completed successfully.");
        }
        OutputFormat::Json | OutputFormat::Jsonl => event::emit_result(format, "flash.run", "ok", serde_json::json!({ "chip": chip }))?,
    }
}
```

- [ ] **Step 2: 验证 `cmd_flash_script` 无需改动**

确认 `cmd_flash_script` 函数（第 333-338 行）已是：

```rust
"ec7xx" | "air8000" | ... => match op {
    "script" => {
        let folders = script_folders.expect("script folder required");
        let script_data = build_script_image_checked(folders, &info)?;
        let boot_port = luatos_flash::ec718::auto_enter_boot_mode(Some(port), &on_progress)?;
        luatos_flash::ec718::flash_script_ec718(soc, &boot_port, &script_data, &on_progress, cancel)?;
    }
    ...
}
```

其中 `build_script_image_checked(folders, &info)` 已支持多目录，无需改动。

- [ ] **Step 3: 验证编译**

```powershell
cargo build -p luatos-cli
cargo clippy -p luatos-cli -- -D warnings
```

---

## Task 4: 修改 GUI 多目录 UI (FlashView.vue)

**Files:**
- Modify: `crates/luatos-gui/src/views/FlashView.vue`

- [ ] **Step 1: 修改数据模型**

在第 35 行附近，将：

```typescript
const scriptFolder = ref('')
```

改为：

```typescript
const scriptFolders = ref<string[]>([])
```

- [ ] **Step 2: 添加删除目录方法**

在 `browseScriptFolder` 方法后添加：

```typescript
function removeScriptFolder(index: number) {
  scriptFolders.value.splice(index, 1)
}
```

- [ ] **Step 3: 修改 `browseScriptFolder` 为添加模式**

将第 76-87 行的 `browseScriptFolder` 函数改为：

```typescript
async function addScriptFolder() {
  try {
    const path = await invoke<string | null>('open_folder_dialog', {
      title: '选择脚本目录',
    })
    if (path && !scriptFolders.value.includes(path)) {
      scriptFolders.value.push(path)
    }
  } catch (e) {
    socError.value = String(e)
  }
}
```

- [ ] **Step 4: 修改传给后端的参数**

将第 114-116 行：

```typescript
const scriptFolders = flashOp.value === 'script' && scriptFolder.value
    ? [scriptFolder.value]
    : null
```

改为：

```typescript
const scriptFoldersParam = flashOp.value === 'script' && scriptFolders.value.length > 0
    ? scriptFolders.value
    : null
```

并更新第 119-124 行的调用：

```typescript
await invoke('flash_run', {
  socPath: socPath.value,
  port: serial.selectedPort,
  baudRate: null,
  scriptFolders: scriptFoldersParam,
})
```

- [ ] **Step 5: 修改 UI 模板**

将第 190-201 行的目录选择区域从单个输入框改为目录列表：

```vue
<!-- 脚本目录选择 (仅刷脚本区时显示) -->
<div v-if="flashOp === 'script'" class="space-y-2">
  <div class="flex flex-wrap gap-2">
    <span v-for="(dir, i) in scriptFolders" :key="i"
      class="flex items-center gap-1 bg-gray-800 rounded px-2 py-1 text-xs text-gray-300">
      {{ dir.split(/[/\\]/).pop() }}
      <button @click="removeScriptFolder(i)" class="text-gray-500 hover:text-red-400 ml-1">×</button>
    </span>
  </div>
  <button @click="addScriptFolder"
    class="px-3 py-1.5 bg-gray-700 hover:bg-gray-600 rounded text-xs text-gray-200 transition-colors">
    + 添加目录
  </button>
</div>
```

- [ ] **Step 6: 修改按钮禁用条件**

将第 209 行的禁用条件从：

```vue
:disabled="!socPath || !serial.selectedPort || (flashOp === 'script' && !scriptFolder)"
```

改为：

```vue
:disabled="!socPath || !serial.selectedPort || (flashOp === 'script' && scriptFolders.length === 0)"
```

- [ ] **Step 7: 验证编译**

```powershell
cd crates/luatos-gui && npm run build
```

---

## Task 5: 最终验证

**Files:**
- No code changes expected

- [ ] **Step 1: 运行仓库检查**

```powershell
cargo fmt --check
cargo clippy --workspace -- -D warnings
cargo test --workspace
```

Expected:
- `cargo fmt --check` passes
- `cargo clippy --workspace -- -D warnings` passes
- `cargo test --workspace` passes

- [ ] **Step 2: 确认工作区干净**

```powershell
git status --short
```

Expected: no unstaged changes.

---

## 改动文件汇总

| Task | 文件 | 改动类型 |
|------|------|---------|
| 1 | `crates/luatos-flash/src/ec718.rs` | 添加 `script_folders` 参数处理逻辑 |
| 2 | `crates/luatos-gui/src-tauri/src/commands/flash.rs` | 传递 `script_folders` 给 ec718 |
| 3 | `crates/luatos-cli/src/cmd_flash.rs` | 传递 `script_folders` 给 ec718 (`cmd_flash_run`) |
| 4 | `crates/luatos-gui/src/views/FlashView.vue` | 多目录 UI |
