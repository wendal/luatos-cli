# HZV 视频转换

`luatos-cli hzv` 将普通视频转成与 AirMaster 兼容的 **HZV v1** 文件。容器封装、
时间戳、索引和校验均由 Rust 实现；媒体编解码使用随包 FFmpeg，运行时无需安装 Python 或 FFmpeg。

## 随包 FFmpeg

完整解压发布包即可使用，程序通过自身位置找到 `tools/ffmpeg/bin/ffmpeg[.exe]`，
不依赖当前工作目录、AirMaster 或 luatos-cli 源码仓库、PATH，也不在使用时下载。
移动程序时一起保留 `tools` 目录，其中也包含 FFmpeg 许可证和来源说明。
macOS universal 包在 `tools/ffmpeg/<架构>/bin` 下保存两份原生 FFmpeg，运行时自动选择。

如需覆盖随包 FFmpeg，查找顺序为：

1. `--ffmpeg <路径>` 显式指定。
2. 环境变量 `LUATOS_FFMPEG`。
3. 随包 `tools/ffmpeg/<架构>/bin/ffmpeg[.exe]` 或 `tools/ffmpeg/bin/ffmpeg[.exe]`。
4. luatos-cli 可执行文件旁的 `ffmpeg[.exe]`。
5. 系统 `PATH` 中的 `ffmpeg`（兼容手动部署）。

无需额外安装 ffprobe。`verify` 完全由 Rust 执行，不需要 FFmpeg。
自行构建时，Windows x64 资源由本项目 Git LFS 提供；其他平台先运行
`python scripts/prepare_ffmpeg.py --target <Rust目标三元组>`（Python 3.11+，仅构建期）。
`cargo build` 会自动将资源复制到产物旁。发布流程按清单校验 SHA256，缺失资源时构建失败，
详见 [随包资源说明](../vendor/ffmpeg/README.md)。

### Windows x64 精简版的范围

随包 Windows x64 FFmpeg 按 HZV 用途裁剪，保留以下常见本地输入：

- 容器：MP4/MOV、MKV/WebM、AVI、FLV、MPEG/MPG、TS/M2TS、WMV/ASF 等。
- 视频：H.264、HEVC/H.265、MPEG-1/2/4、VP8/VP9、WMV/VC-1、MJPEG、ProRes 等。
- 音频：AAC、MP3/MP2、PCM/ADPCM、Vorbis、Opus、FLAC、ALAC、AC-3/E-AC-3、WMA 等。
- 输出：MJPEG、MP3（LAME）、PCM S16LE；保留 HZV 所需缩放、调帧率、补边、
  裁剪、音频重采样和补静音，以及输入自动旋转所需滤镜。

去掉网络、设备采集、硬件加速、字幕渲染和通用视频输出编码器；AV1 等未列入白名单的
编码不包含在精简版中。容器扩展名不能代表内部编码，遇到未启用的编码可用
`--ffmpeg` 指定完整 FFmpeg。其他平台发布包目前仍使用原有上游静态构建。

精简构建的源码版本、SHA256 和组件白名单均保存在项目中，重建步骤见
[随包资源说明](../vendor/ffmpeg/README.md)。

## 基本用法

```powershell
# 探测视频
luatos-cli hzv probe input.mp4

# 默认 480×320、10 FPS、JPEG q=5、MP3 32kHz/单声道/64kbps
# 不指定 -o 时在源文件旁生成同名 .hzv
luatos-cli hzv convert input.mp4 -o output.hzv

# 分辨率、帧率、清晰度、音质与画面适配
luatos-cli hzv convert input.mp4 -o output.hzv --width 800 --height 480 --fps 24 --video-quality 3 --scale-mode cover --audio-rate 44100 --audio-channels 2 --audio-bitrate-kbps 128

# 分数帧率、截取片段、循环提示、允许覆盖
luatos-cli hzv convert input.mp4 --fps 30000/1001 --start-seconds 5 --duration-seconds 10 --loop-hint --overwrite

# 无声视频；也可以用 --audio-codec pcm_s16le 输出 16 位小端 PCM
luatos-cli hzv convert silent.mp4 --audio-codec none

# 容器校验；--deep 额外检查媒体负载 CRC32 与 JPEG 起止标记
luatos-cli hzv verify output.hzv --deep

# AI / 脚本调用
luatos-cli --format json hzv probe input.mp4
luatos-cli --format jsonl hzv convert input.mp4 -o output.hzv
```

`--format json` 的 stdout 只输出一份标准结果信封；`jsonl` 先输出 `progress`
事件，最后输出 `result`，失败输出 `error` 并以非零退出码结束。
阶段进度为探测、视频转码、音频转码、封装、校验、完成，不是 FFmpeg 逐帧进度。

## 参数与轨道结束策略

- 宽高均为 16..4096 的偶数；帧率大于 0 且不超过 60，支持整数和分数。
- `--video-quality` 为 FFmpeg `q:v`，范围 2..31，越小画质越高、文件通常越大。
- `--scale-mode contain` 等比缩放并补边（`--background black|white`）；
  `cover` 等比放大裁剪；`stretch` 拉伸到目标尺寸。
- 音频编码为 `mp3`、`pcm_s16le` 或 `none`。默认请求 MP3；源视频无音轨时应显式选 `none`。
- MP3 音频包默认目标时长 80 ms，按完整 MP3 帧聚合，因此实际包时长可略长。
- `--end-policy hold-video`（默认）：保留两条轨道，总时长取较长值；末帧保持由播放器实现。
- `--end-policy shortest`：只保留开始时间小于短轨道结束时间的媒体包，
  容器总时长取短轨道时长；边界处不拆分 JPEG/MP3 包，与 AirMaster 一致。
- `--end-policy pad-audio`：按截取时长或探测到的剩余时长补静音；无法探测时长时必须提供 `--duration-seconds`。
- 默认开启媒体包 CRC32；`--no-packet-crc` 可关闭，文件头/索引/文件尾校验始终保留。
- `--loop-hint` 只写循环提示，不复制媒体内容。
- 已有输出默认拒绝覆盖；`--overwrite` 也会在新文件完成转码和深度校验后才替换目标。

## 二进制布局

所有整数为小端序，时间基为 1000 ticks/s。视频为逐帧完整 JPEG，音频为 MP3 或 PCM；
文件头中的 RGB565 是播放端输出提示，不是媒体负载编码。

```text
128 字节 HZV1 文件头
音视频媒体包（按 PTS 排序，相同时间戳音频优先）：
    32 字节 HZPK 包头 + 负载 + 0..3 字节零填充
32 字节 HZIX 索引头 + N × 24 字节索引项
32 字节 HZTL 文件尾
```

视频每帧建立索引，音频约每秒建立索引。包头包含流类型、标志、PTS、包时长、
负载长度、负载 CRC32、包序号。输出包含文件 SHA256，便于比对与分发。

校验检查结构边界、包数量、轨道内 PTS 单调性、索引对应关系、完整的视频索引、
文件头/索引/文件尾 CRC32；深度模式还检查媒体负载 CRC32 和 JPEG 边界，
不进行完整音视频解码。
