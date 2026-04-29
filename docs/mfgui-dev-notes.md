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

本机 Cargo 缓存（`~/.cargo/registry/src/`）中无 egui 0.34 相关包，
而 egui 0.34 依赖 `winit 0.30.x` 等，这些均不在缓存中，必须从网络下载。

### 解决办法

配置国内 Cargo 镜像（rsproxy.cn）：在 `~/.cargo/config.toml` 中添加：

```toml
[source.crates-io]
replace-with = "rsproxy-sparse"

[source.rsproxy-sparse]
registry = "sparse+https://rsproxy.cn/index/"

[net]
git-fetch-with-cli = true
```

此文件不纳入版本库（属于个人开发机配置），配置后即可正常下载 egui 0.34。

### 经验总结

> 在弱网/离线环境添加新 GUI 依赖时，优先配置 rsproxy.cn 镜像，而非降级到旧版本。

---

## 坑2：eframe 0.27 → 0.34 API 差异

### 差异对照表

| 位置 | 0.27 写法 | 0.34 写法 |
|------|-----------|-----------|
| `AppCreator` 返回值 | `Box::new(\|cc\| Box::new(App::new(cc)))` | `Box::new(\|cc\| Ok(Box::new(App::new(cc))))` |
| `App` trait 必须实现的方法 | `fn update(&mut self, ctx, frame)` | `fn ui(&mut self, ui: &mut egui::Ui, frame)` |
| 非绘制逻辑钩子 | 无，写在 `update` 开头 | `fn logic(&mut self, ctx, frame)`（可选） |
| `font_data` HashMap 值类型 | `FontData` | `Arc<FontData>` |

### 解决办法

1. `main.rs`：`AppCreator` 闭包返回 `Ok(Box::new(...))`
2. `app.rs`：将原 `update` 中的非绘制逻辑（worker 轮询、`request_repaint`）移到 `logic()`
3. `app.rs`：将原 `update` 中的 UI 代码（去掉 `CentralPanel::default().show(ctx, ...)` 外层）写到 `fn ui`
4. 字体插入：`Arc::new(egui::FontData::from_owned(bytes))`

---

## 坑3：rfd 文件对话框在 Linux 环境拉取 ashpd 失败

### 现象

添加 `rfd = "0.15"` 后，Linux 环境编译失败，提示需要下载 `ashpd`（xdg-portal 绑定）。

### 解决办法

```toml
rfd = { version = "0.15", default-features = false }
```

Windows 上无需 xdg-portal，关闭默认 feature 后直接使用 WinAPI 原生对话框。

---

## 坑4：egui 中文字符显示乱码（方块）

### 现象

程序运行后，所有中文标签、按钮文字均显示为空白方块（□□□）。

### 根本原因

egui 内置的默认字体（`NotoSans`）**仅包含 Latin 字符集**，不含 CJK（汉字、假名等）。

### 解决办法

在 `MfguiApp::new(cc)` 里调用字体加载函数，从 Windows 系统字体目录加载支持 CJK 的字体：

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
            // egui 0.34: font_data 值为 Arc<FontData>
            fonts.font_data.insert("cjk".to_owned(), Arc::new(egui::FontData::from_owned(bytes)));
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

---

## 架构说明

```
luatos-mfgui/
├── Cargo.toml        — 依赖：egui 0.34 + eframe 0.34 + rfd 0.15（无默认 features）
└── src/
    ├── main.rs       — eframe 入口，窗口标题含版本号
    ├── app.rs        — MfguiApp（egui App trait），logic(轮询)+ui(渲染)+字体加载
    ├── state.rs      — AppState 纯逻辑（无 egui 依赖），含 15 个单元测试
    └── worker.rs     — 后台刷机线程，mpsc channel 消息，含 3 个单元测试
```

**设计原则：**
- `state.rs` 不依赖任何 GUI 库，所有业务逻辑可独立单元测试
- `worker.rs` 通过 `Arc<AtomicBool>` 实现取消，`ProgressCallback` 克隆发送到 channel
- `app.rs` 只做状态展示与事件派发，`poll_worker()` 在每帧 `logic()` 中非阻塞轮询

