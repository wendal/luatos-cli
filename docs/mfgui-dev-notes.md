# luatos-mfgui 开发笔记

## 背景

为满足生产线量产需求，新增基于 egui 的独立刷机 GUI 工具 `luatos-mfgui`。
本文记录开发过程中踩到的坑，供后续维护参考。

---

## 坑1：egui 版本与本地 Cargo 缓存不匹配

### 现象

离线/弱网环境下，首次指定 `egui = "0.34"` 后 `cargo build` 长时间卡死，
随后报错：`network timeout` 或 `error: failed to download`。

### 根本原因

本机 Cargo 缓存（`~/.cargo/registry/src/`）中只有 **egui 0.27.2 / eframe 0.27.2**，
而 egui 0.34 依赖 `winit 0.30.x`（及间接的 `orbclient`），这些均不在缓存中，
必须从网络下载，在弱网环境下必然失败。

### 解决办法

1. 先枚举本地缓存中已有的版本：
   ```powershell
   Get-ChildItem "$env:USERPROFILE\.cargo\registry\src\index.crates.io-*" -Directory |
       Where-Object { $_.Name -match "^egui-" } | Select-Object Name
   ```
2. 将 `Cargo.toml` 中的版本锁定到缓存中已有的版本，例如：
   ```toml
   egui  = "0.27"
   eframe = { version = "0.27", default-features = false, features = ["default_fonts", "glow"] }
   ```

### 经验总结

> 在弱网/离线环境添加新 GUI 依赖时，**务必先确认本地缓存中已有的版本**，
> 再写入 `Cargo.toml`，否则即使版本号合法也会因网络问题导致编译失败。

---

## 坑2：eframe AppCreator 跨版本 API 不兼容

### 现象

将 egui 从 0.34 降级到 0.27 后，编译报错：

```
error[E0308]: mismatched types
   --> src/main.rs:34:23
    |
    |     Box::new(|cc| Ok(Box::new(app::MfguiApp::new(cc)))),
    |                   ^^ expected `Box<dyn App>`, found `Result<Box<dyn App>>`
```

### 根本原因

`AppCreator` 类型定义在两个版本之间不同：

| 版本 | `AppCreator` 定义 |
|------|------------------|
| 0.27 | `Box<dyn FnOnce(&CreationContext) -> Box<dyn App>>` |
| 0.34 | `Box<dyn FnOnce(&CreationContext) -> Result<Box<dyn App>>>` |

### 解决办法

```rust
// 0.34（错误，0.27 不兼容）
Box::new(|cc| Ok(Box::new(app::MfguiApp::new(cc))))

// 0.27（正确）
Box::new(|cc| Box::new(app::MfguiApp::new(cc)))
```

---

## 坑3：rfd 文件对话框在 Linux 环境拉取 ashpd 失败

### 现象

添加 `rfd = "0.15"` 后，Linux 环境编译失败，提示需要下载 `ashpd`（xdg-portal 绑定）。

### 根本原因

rfd 的默认 features 在 Linux 上包含 `xdg-portal` / `gtk3`，会引入 `ashpd` 等重量级依赖。

### 解决办法

```toml
rfd = { version = "0.15", default-features = false }
```

Windows 上无需 xdg-portal，关闭默认 feature 后直接使用 WinAPI 原生对话框，
功能完整，体积更小。

---

## 坑4：egui 中文字符显示乱码（方块）

### 现象

程序运行后，所有中文标签、按钮文字均显示为空白方块（□□□）。

### 根本原因

egui 内置的默认字体（`NotoSans`）**仅包含 Latin 字符集**，不含 CJK（汉字、假名等）。
当渲染找不到对应字形时，显示替代方块。

### 解决办法

在 `app.rs` 中，于 `MfguiApp::new(cc)` 里调用字体加载函数，
从 Windows 系统字体目录加载支持 CJK 的字体，通过 `ctx.set_fonts()` 注入：

```rust
fn load_cjk_font(ctx: &egui::Context) {
    let candidates = &[
        r"C:\Windows\Fonts\msyh.ttc",   // 微软雅黑（首选）
        r"C:\Windows\Fonts\simsun.ttc", // 宋体
        r"C:\Windows\Fonts\simhei.ttf", // 黑体
    ];
    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            let mut fonts = egui::FontDefinitions::default();
            fonts.font_data.insert("cjk".to_owned(), egui::FontData::from_owned(bytes));
            fonts.families.entry(egui::FontFamily::Proportional)
                .or_default().insert(0, "cjk".to_owned());
            fonts.families.entry(egui::FontFamily::Monospace)
                .or_default().push("cjk".to_owned());
            ctx.set_fonts(fonts);
            return;
        }
    }
}
```

**关键点：**
- 字体插入到 `Proportional` 字体栈的 **第 0 位**（最高优先级），
  使 egui 优先用该字体渲染所有文字（中英文均覆盖）。
- 同时 `push` 到 `Monospace` 字体栈末尾，作为日志区的汉字兜底。
- 加载失败时静默回退，程序仍可正常启动（中文显示为方块但不崩溃）。

### 跨平台推广

macOS / Linux 可在 `candidates` 中追加对应路径：

```rust
// macOS
"/System/Library/Fonts/PingFang.ttc",
"/Library/Fonts/Arial Unicode.ttf",

// Linux
"/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
"/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc",
```

---

## 架构说明

```
luatos-mfgui/
├── Cargo.toml        — 依赖：egui 0.27 + eframe 0.27 + rfd 0.15（无默认 features）
└── src/
    ├── main.rs       — eframe 入口，窗口标题含版本号
    ├── app.rs        — MfguiApp（egui App trait），UI 渲染 + 字体加载
    ├── state.rs      — AppState 纯逻辑（无 egui 依赖），含 12 个单元测试
    └── worker.rs     — 后台刷机线程，mpsc channel 消息，含 3 个单元测试
```

**设计原则：**
- `state.rs` 不依赖任何 GUI 库，所有业务逻辑可独立单元测试
- `worker.rs` 通过 `Arc<AtomicBool>` 实现取消，`ProgressCallback` 克隆发送到 channel
- `app.rs` 只做状态展示与事件派发，`poll_worker()` 在每帧 `update()` 中非阻塞轮询
