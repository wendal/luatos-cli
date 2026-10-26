//! HZV 命令入口，媒体与容器实现由 luatos-hzv 复用。

use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::{Args, Subcommand};
use luatos_hzv::{ConvertOptions, FrameRate};
use serde_json::json;

use crate::{event, OutputFormat};

#[derive(Subcommand)]
pub(crate) enum HzvCommands {
    /// 探测源视频的时长、分辨率与音频参数
    Probe { input: PathBuf },
    /// 将普通视频转为 HZV v1（MJPEG + MP3/PCM）
    Convert(Box<ConvertArgs>),
    /// 校验 HZV 结构、索引和时间戳
    Verify {
        input: PathBuf,
        /// 额外校验媒体负载 CRC32 和 JPEG 帧边界
        #[arg(long)]
        deep: bool,
    },
}

#[derive(Args)]
pub(crate) struct ConvertArgs {
    /// 源视频路径
    input: PathBuf,
    /// 输出路径，默认将源文件扩展名改为 .hzv
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// 输出宽度，16..4096 的偶数
    #[arg(long, default_value_t = 480)]
    width: u16,
    /// 输出高度，16..4096 的偶数
    #[arg(long, default_value_t = 320)]
    height: u16,
    /// 输出帧率，支持整数或 30000/1001 等分数，最大 60
    #[arg(long, default_value = "10")]
    fps: FrameRate,
    /// FFmpeg JPEG 质量参数，2..31，数值越小画质越高
    #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u8).range(2..=31))]
    video_quality: u8,
    /// contain=等比补背景；cover=等比裁剪；stretch=拉伸
    #[arg(long, default_value = "contain", value_parser = ["contain", "cover", "stretch"])]
    scale_mode: String,
    /// 补边背景色
    #[arg(long, default_value = "black", value_parser = ["black", "white"])]
    background: String,
    /// 输出音频编码；无音轨的源视频须选择 none
    #[arg(long, default_value = "mp3", value_parser = ["mp3", "pcm_s16le", "none"])]
    audio_codec: String,
    /// 音频采样率（Hz）
    #[arg(long, default_value_t = 32000)]
    audio_rate: u32,
    /// 音频声道数
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u16).range(1..=2))]
    audio_channels: u16,
    /// MP3 码率（kbps）
    #[arg(long, default_value_t = 64)]
    audio_bitrate_kbps: u16,
    /// 目标音频包时长（毫秒），MP3 保留完整帧边界
    #[arg(long, default_value_t = 80)]
    audio_packet_ms: u32,
    /// 从指定秒数开始转换
    #[arg(long, default_value_t = 0.0)]
    start_seconds: f64,
    /// 仅转换指定时长（秒）
    #[arg(long)]
    duration_seconds: Option<f64>,
    /// 轨道结尾处理：保持末帧、按短轨道结束或补静音
    #[arg(long, default_value = "hold-video", value_parser = ["hold-video", "shortest", "pad-audio"])]
    end_policy: String,
    /// 关闭媒体包 CRC32（文件头、索引和文件尾始终校验）
    #[arg(long)]
    no_packet_crc: bool,
    /// 写入循环播放提示
    #[arg(long)]
    loop_hint: bool,
    /// 允许替换已有输出文件
    #[arg(long)]
    overwrite: bool,
}

impl ConvertArgs {
    fn options(&self, ffmpeg: Option<&Path>) -> ConvertOptions {
        ConvertOptions {
            input: self.input.clone(),
            output: self.output.clone().unwrap_or_else(|| self.input.with_extension("hzv")),
            width: self.width,
            height: self.height,
            fps: self.fps,
            video_quality: self.video_quality,
            scale_mode: self.scale_mode.clone(),
            background: self.background.clone(),
            audio_codec: self.audio_codec.clone(),
            audio_rate: self.audio_rate,
            audio_channels: self.audio_channels,
            audio_bitrate_kbps: self.audio_bitrate_kbps,
            audio_packet_ms: self.audio_packet_ms,
            start_seconds: self.start_seconds,
            duration_seconds: self.duration_seconds,
            end_policy: self.end_policy.clone(),
            packet_crc: !self.no_packet_crc,
            loop_hint: self.loop_hint,
            overwrite: self.overwrite,
            ffmpeg: ffmpeg.map(Path::to_owned),
        }
    }
}

pub(crate) fn run(action: &HzvCommands, ffmpeg: Option<&Path>, format: &OutputFormat) -> Result<()> {
    match action {
        HzvCommands::Probe { input } => {
            let result = luatos_hzv::probe(input, &luatos_hzv::find_ffmpeg(ffmpeg))?;
            if *format == OutputFormat::Text {
                println!("{}", serde_json::to_string_pretty(&result)?);
            }
            event::emit_result(format, "hzv.probe", "ok", result)
        }
        HzvCommands::Convert(args) => {
            let result = luatos_hzv::convert(&args.options(ffmpeg), |progress| {
                if *format == OutputFormat::Text {
                    eprintln!("[{:>6.1}%] {}", progress.progress * 100.0, progress.message);
                }
                event::emit_jsonl_event(
                    format,
                    json!({ "type":"progress", "command":"hzv.convert", "stage":progress.stage, "progress":progress.progress, "message":progress.message }),
                )
            })?;
            if *format == OutputFormat::Text {
                println!("HZV 转换完成：{}", result.output.display());
                println!(
                    "  {} × {}，{}/{} FPS，{} 帧，{} 毫秒，{} 字节",
                    result.header.video_width,
                    result.header.video_height,
                    result.header.video_fps_num,
                    result.header.video_fps_den,
                    result.video_packets,
                    result.duration_ms,
                    result.size
                );
                println!("  SHA256: {}", result.sha256);
            }
            event::emit_result(format, "hzv.convert", "ok", result)
        }
        HzvCommands::Verify { input, deep } => {
            let result = luatos_hzv::verify(input, *deep)?;
            if *format == OutputFormat::Text {
                println!("HZV 校验通过：{}（视频 {} 帧，音频 {} 包）", input.display(), result.video_packets, result.audio_packets);
            }
            event::emit_result(format, "hzv.verify", "ok", result)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cli, Commands};
    use clap::Parser;

    #[test]
    fn parses_hzv_defaults_fractional_fps_and_global_options() {
        let cli = Cli::try_parse_from(["luatos-cli", "hzv", "convert", "demo.mp4", "--ffmpeg", "ffmpeg.exe", "--format", "jsonl"]).unwrap();
        let Commands::Hzv {
            action: HzvCommands::Convert(args),
            ffmpeg,
        } = cli.command
        else {
            panic!("错误的命令分发")
        };
        let options = args.options(ffmpeg.as_deref());
        assert_eq!(options.output, PathBuf::from("demo.hzv"));
        assert_eq!((options.width, options.height, options.audio_rate), (480, 320, 32000));
        assert!(options.packet_crc);
        let cli = Cli::try_parse_from(["luatos-cli", "hzv", "convert", "demo.mp4", "-o", "out.hzv", "--fps", "30000/1001", "--audio-codec", "none"]).unwrap();
        let Commands::Hzv {
            action: HzvCommands::Convert(args),
            ..
        } = cli.command
        else {
            panic!("错误的命令分发")
        };
        assert_eq!(args.fps, FrameRate { num: 30000, den: 1001 });
        assert!(Cli::try_parse_from(["luatos-cli", "hzv", "convert", "demo.mp4", "--fps", "0"]).is_err());
    }
}
