# EC7xx `--script-only` FOTA 设计说明

## 背景

当前 `luatos-cli fota build` 对 EC7xx / Air780EPM / Air780EHM / Air8000 只支持差分 FOTA：必须同时提供 `--new` 和 `--old`，并通过 `FotaToolkit.exe` 生成 `delta.par` 后再组装为 `.sota`。

参考仓库 `luatools_py3` 里，`build_fota(script_only=True)` 已经支持“只生成脚本升级包”的语义：只依赖 `script.bin`，不要求 `app_pack.rbl`。本次要把这个能力补到当前仓库的 EC7xx 分支中，且保持纯 Rust 实现，不引入 Python 依赖。

## 目标

1. EC7xx / Air780EPM / Air780EHM / Air8000 支持 `--script-only`。
2. `--script-only` 不再要求 `--old`。
3. 继续沿用当前 CLI 的 `.sota` 输出习惯，不新增 Python 依赖。
4. 对缺少脚本区、缺少脚本地址、芯片不匹配等情况给出明确错误。
5. 同步更新文档与示例。

## 非目标

1. 不实现 EC7xx 的全量 FOTA。
2. 不改动现有差分 FOTA 的包格式与外部工具调用方式。
3. 不在本次范围内重构整个 `fota build` 的芯片分派架构。

## 方案

### 推荐方案：EC7xx 增加独立脚本-only builder

在 `crates/luatos-cli/src/cmd_fota.rs` 中为 EC7xx 增加一个独立的脚本-only 生成路径：

- 读取新 `.soc`
- 校验 `download.script_addr`
- 检查 `script.bin` 是否存在
- 用 `luatos-soc::ota::lzma_compress_file()` 压缩脚本区
- 用现有 `luatos-soc::ota::assemble_ota_package()` 组装输出

这样可以保持当前 `.sota` 产物外壳不变，只改变内容组成。

### 备选方案：复用差分流程，只塞空 SDK

仍走差分 FOTA 的外壳，只是将 SDK 侧固定为空或占位数据。

优点是改动更少；缺点是语义不清晰，后续维护时容易混淆“脚本-only”和“差分包”。

### 不推荐方案：引入新脚本包格式

直接仿照旧工具输出一个新的脚本包格式。

优点是可完全对齐历史行为；缺点是会分裂 CLI 产物语义，且要额外处理设备侧兼容性。

## 设计

### 命令行为

当 `chip_type` 属于以下集合时：

- `ec7xx`
- `ec618`
- `air8000`
- `air780epm`
- `air780ehm`
- `air780ehv`
- `air780ehg`
- `air780epv`

并且用户传入 `--script-only` 时：

1. 不再要求 `--old`
2. 忽略差分构建路径
3. 生成仅包含脚本分区内容的升级包

### 输入校验

脚本-only 生成前必须满足：

- `.soc` 文件存在
- `download.script_addr` 存在且可解析
- `script.bin` 存在
- `chip.type` 属于 EC7xx 兼容集合

任一条件不满足时直接返回错误，不做静默降级。

### 输出

输出仍使用当前 `.sota` 命名规则：

- 默认输出：`<chip>_fota.sota`
- `--output` 指定时使用用户路径

输出内容由“脚本区压缩数据 + OTA 包头”构成，SDK 侧不携带差分数据。

## 实现拆分

1. `cmd_fota.rs`
   - 新增 `build_ec7xx_script_only_fota()`
   - 在 EC7xx 分支内根据 `script_only` 分流
   - 调整错误提示，避免再强制提示 `--old`

2. `luatos-soc::ota`
   - 复用现有 `lzma_compress_file()`
   - 复用现有 `assemble_ota_package()`
   - 如现有头部字段不足以表达“脚本-only”，再补最小辅助函数，但不新增外部依赖

3. 文档
   - `README.md`
   - `docs/ota-guide.md`
   - `docs/models/air8000-ec7xx.md`

4. 测试
   - 成功路径：EC7xx `--script-only` 生成可用文件
   - 失败路径：缺 `script.bin`、缺 `download.script_addr`、缺 `.soc` 文件
   - 回归路径：EC7xx 差分 FOTA 行为不变

## 验收标准

1. `luatos-cli fota build --new <ec7xx.soc> --script-only` 可成功生成文件。
2. 不传 `--old` 时不会报“Full FOTA not supported”。
3. EC7xx 差分 FOTA 仍可正常工作。
4. 文档中能查到 EC7xx 的 `--script-only` 用法。
5. 全程不引入 Python 运行时或 Python 依赖。

