# 随包 FFmpeg

运行时资源布局为 `tools/ffmpeg/bin/ffmpeg[.exe]`，相对于 luatos-cli 可执行文件，
不访问源码仓库、不自动下载，也不要求用户安装 FFmpeg 或配置 PATH。

- Windows x64 的静态 FFmpeg 已纳入本项目 Git LFS，版本、来源和配置见对应目录
  `FFmpeg-README.txt`；版权与许可证见 `FFmpeg-LICENSE.txt`。
- Windows x64 使用 HZV 精简构建，源版本与 SHA256 锁定于
  `scripts/ffmpeg-minimal-sources.json`，具体组件白名单见 `scripts/build_hzv_ffmpeg.sh`。
  其他平台目前保留上游静态构建。
- 其他发布平台由 `scripts/prepare_ffmpeg.py` 按 `manifest.json` 中的固定版本、
  下载地址与 SHA256 准备静态程序及上游声明。下载只发生在构建/发布阶段。
- `crates/luatos-cli/build.rs` 将本平台程序和声明自动复制到构建产物旁；发布流程
  设置 `LUATOS_REQUIRE_BUNDLED_FFMPEG=1`，资源缺失时禁止继续发布。
- macOS universal 包保留 `tools/ffmpeg/x86_64` 和 `tools/ffmpeg/aarch64` 两套资源，
  根据实际运行的可执行切片选择对应架构。

```bash
# 已拉取 LFS 的 Windows x64 可直接 cargo build，无需准备或下载。
git lfs pull --include='vendor/ffmpeg/**'
python scripts/prepare_ffmpeg.py --target x86_64-pc-windows-msvc
cargo build -p luatos-cli
```

更新第三方版本时，应同时更新锁定的 SHA256、上游来源及许可证；不能使用浮动
`latest` 下载地址。BtbN ARM64 构建锁定月末保留的发布标签。

## 重新构建 Windows x64 精简版

需要 Python 3.12+、MSYS2 UCRT64 GCC、MSYS make、pkg-config、strip。
脚本下载并校验固定版本的 FFmpeg/LAME 源码及便携 NASM；编译产物只写入工作目录，
不自动覆盖已随包的程序：

```powershell
python scripts/build_hzv_ffmpeg.py --bash E:/tools/msys64/usr/bin/bash.exe
# 可用 --work-dir 指定不含空格的构建目录
```

结果位于 `target/ffmpeg-minimal/result/`，包含 `bin/ffmpeg.exe`、许可证、来源与配置。
构建可使用 `HZV_BUILD_JOBS` 控制并行数；修改构建配置后设置 `HZV_RECONFIGURE=1`
或使用新的工作目录重新配置。此过程不需要改动系统中的 FFmpeg。

发布替换前应运行 `scripts/smoke_hzv_bundle.py`，验证预生成的 12 个常见媒体输入，
并更新 `manifest.json` 中的二进制和声明 SHA256。
