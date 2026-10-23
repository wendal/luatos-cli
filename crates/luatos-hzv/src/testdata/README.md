# AirMaster HZV v1 兼容性夹具

`airmaster-v1.hzv` 由原项目 `tools/hzv/hzv_converter.py` 生成，
输入为 FFmpeg lavfi 的 1 秒 `testsrc2` 测试图与 440 Hz 正弦音，无外部媒体素材。

- 视频：32×24、10 FPS、MJPEG、q=5、10 帧。
- 音频：MP3、32000 Hz、单声道、64 kbps、80 ms 目标包时长。
- 默认 hold-video，开启媒体包 CRC32，关闭循环提示。

内联测试独立读取此夹具，再由 Rust 重新封装，逐字节比较文件头前 108 字节以及
128 字节后的全部媒体包、索引与文件尾。内容 ID 和依赖它的文件头 CRC 允许不同。
正常测试不需要 Python、FFmpeg 或 AirMaster 源码。
