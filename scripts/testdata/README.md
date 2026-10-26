# HZV 随包验证输入

`hzv-source.mp4` 是 FFmpeg 生成的 1 秒合成测试图与 440 Hz 正弦音，
视频为 64×48、10 FPS、MPEG-4，音频为 32000 Hz AAC，不含外部媒体素材。

精简 FFmpeg 只保留 MJPEG/MP3/PCM 输出，不再携带测试图/正弦音生成器或
MPEG-4/AAC 输出编码器，因此使用这个小型固定输入执行脱离仓库的转码验证。

`media/` 同样由 FFmpeg 的 `testsrc2` 与 `sine` 生成，不含外部素材，用于验证常见输入：

| 文件 | 视频 / 音频 |
|---|---|
| h264.mp4 | H.264 / AAC |
| hevc.mp4 | HEVC / AAC |
| prores.mov | ProRes 10 bit / PCM |
| vp8.webm | VP8 / Vorbis |
| vp9.webm | VP9 / Opus |
| mpeg2.ts | MPEG-2 / MP2 |
| mpeg1.mpg | MPEG-1 / MP2 |
| wmv.wmv | WMV2 / WMA2 |
| mjpeg.avi | MJPEG / PCM |
| flv.flv | FLV1 / MP3 |
| h264-flac.mkv | H.264 / FLAC |

每个输入均执行探测、默认参数转换、深度校验。全程使用迁移后的发布目录与空 PATH，
不能依赖仓库路径或系统 FFmpeg；生成测试输入的完整 FFmpeg 不参与验证过程。
另外验证无音频、PCM/双声道、三种缩放模式、分数帧率、关闭包 CRC、循环提示、
片段截取、补静音和按短轨道结束等参数组合，并深度校验每个输出。
