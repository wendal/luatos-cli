//! HZV v1 转换库。Rust 负责容器与校验，FFmpeg 负责媒体编解码。

mod format;
mod media;

use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{ensure, Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};

pub use format::{verify, Header, Verification};
pub use media::{find_ffmpeg, probe, ProbeResult};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameRate {
    pub num: u32,
    pub den: u32,
}

impl Default for FrameRate {
    fn default() -> Self {
        Self { num: 10, den: 1 }
    }
}

impl FromStr for FrameRate {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self> {
        let (num, den) = value.split_once('/').unwrap_or((value, "1"));
        let fps = Self {
            num: num.parse().context("帧率分子必须是正整数")?,
            den: den.parse().context("帧率分母必须是正整数")?,
        };
        ensure!(fps.num > 0 && fps.den > 0 && fps.num as u64 <= 60 * fps.den as u64, "帧率必须大于 0 且不超过 60");
        // 极低帧率不适合本容器的 u32 包时长字段。
        ensure!(1000 * fps.den as u64 / fps.num as u64 <= u32::MAX as u64, "帧率过低");
        Ok(fps)
    }
}

#[derive(Clone, Debug)]
pub struct ConvertOptions {
    pub input: PathBuf,
    pub output: PathBuf,
    pub width: u16,
    pub height: u16,
    pub fps: FrameRate,
    pub video_quality: u8,
    pub scale_mode: String,
    pub background: String,
    pub audio_codec: String,
    pub audio_rate: u32,
    pub audio_channels: u16,
    pub audio_bitrate_kbps: u16,
    pub audio_packet_ms: u32,
    pub start_seconds: f64,
    pub duration_seconds: Option<f64>,
    pub end_policy: String,
    pub packet_crc: bool,
    pub loop_hint: bool,
    pub overwrite: bool,
    pub ffmpeg: Option<PathBuf>,
}

impl Default for ConvertOptions {
    fn default() -> Self {
        Self {
            input: PathBuf::new(),
            output: PathBuf::new(),
            width: 480,
            height: 320,
            fps: FrameRate::default(),
            video_quality: 5,
            scale_mode: "contain".into(),
            background: "black".into(),
            audio_codec: "mp3".into(),
            audio_rate: 32000,
            audio_channels: 1,
            audio_bitrate_kbps: 64,
            audio_packet_ms: 80,
            start_seconds: 0.0,
            duration_seconds: None,
            end_policy: "hold-video".into(),
            packet_crc: true,
            loop_hint: false,
            overwrite: false,
            ffmpeg: None,
        }
    }
}

impl ConvertOptions {
    pub fn validate(&self) -> Result<()> {
        ensure!(self.input.is_file(), "源文件不存在：{}", self.input.display());
        ensure!(!self.output.as_os_str().is_empty(), "必须指定输出文件");
        if self.output.exists() {
            ensure!(self.input.canonicalize()? != self.output.canonicalize()?, "输出不能与源文件相同");
            ensure!(self.overwrite, "输出文件已存在，需指定 --overwrite：{}", self.output.display());
            ensure!(self.output.is_file(), "输出路径不是普通文件");
        }
        ensure!((16..=4096).contains(&self.width) && (16..=4096).contains(&self.height), "宽高必须在 16..4096 之间");
        ensure!(self.width.is_multiple_of(2) && self.height.is_multiple_of(2), "yuvj420p 输出宽高必须为偶数");
        format!("{}/{}", self.fps.num, self.fps.den).parse::<FrameRate>()?;
        ensure!((2..=31).contains(&self.video_quality), "JPEG 质量参数必须在 2..31 之间");
        ensure!(["contain", "cover", "stretch"].contains(&self.scale_mode.as_str()), "无效的画面适配方式");
        ensure!(["black", "white"].contains(&self.background.as_str()), "背景色仅支持 black/white");
        ensure!(["mp3", "pcm_s16le", "none"].contains(&self.audio_codec.as_str()), "无效的音频编码");
        ensure!(["hold-video", "shortest", "pad-audio"].contains(&self.end_policy.as_str()), "无效的结尾处理方式");
        ensure!((8000..=192000).contains(&self.audio_rate) && (1..=2).contains(&self.audio_channels), "音频采样率或声道无效");
        if self.audio_codec == "mp3" {
            ensure!(
                [8000, 11025, 12000, 16000, 22050, 24000, 32000, 44100, 48000].contains(&self.audio_rate),
                "MP3 采样率不受支持"
            );
        }
        ensure!(
            (8..=320).contains(&self.audio_bitrate_kbps) && (10..=1000).contains(&self.audio_packet_ms),
            "音频码率或包时长超出范围"
        );
        ensure!(self.start_seconds.is_finite() && self.start_seconds >= 0.0, "开始时间必须是非负有限数值");
        ensure!(self.duration_seconds.is_none_or(|v| v.is_finite() && v > 0.0), "截取时长必须是正有限数值");
        Ok(())
    }
}

#[derive(Debug, Serialize)]
pub struct Progress {
    pub stage: &'static str,
    pub progress: f64,
    pub message: &'static str,
}

#[derive(Debug, Serialize)]
pub struct ConvertResult {
    pub output: PathBuf,
    pub size: u64,
    pub packet_count: u32,
    pub video_packets: u32,
    pub audio_packets: u32,
    pub index_entries: u32,
    pub duration_ms: u64,
    pub sha256: String,
    pub header: Header,
}

fn duration(packets: &[format::Packet]) -> u64 {
    packets.last().map(|p| p.pts + p.duration as u64).unwrap_or(0)
}

fn apply_end_policy(video: &mut Vec<format::Packet>, audio: &mut Vec<format::Packet>, policy: &str) -> u64 {
    let (v, a) = (duration(video), duration(audio));
    if audio.is_empty() {
        return v;
    }
    if policy != "shortest" {
        return v.max(a);
    }
    let cutoff = v.min(a);
    video.retain(|p| p.pts < cutoff);
    audio.retain(|p| p.pts < cutoff);
    for packets in [video, audio] {
        if let Some(last) = packets.last_mut() {
            last.flags |= format::END;
        }
    }
    cutoff
}

/// 转码、封装并深度校验，全部成功后原子替换目标文件。
pub fn convert(options: &ConvertOptions, mut progress: impl FnMut(&Progress) -> Result<()>) -> Result<ConvertResult> {
    options.validate()?;
    let input = options.input.canonicalize()?;
    let ffmpeg = find_ffmpeg(options.ffmpeg.as_deref());
    progress(&Progress {
        stage: "probe",
        progress: 0.02,
        message: "探测源媒体",
    })?;
    let media = probe(&input, &ffmpeg)?;
    ensure!(media.video.is_some(), "源文件没有视频轨道");
    ensure!(options.audio_codec == "none" || media.audio.is_some(), "源文件没有音频轨道，请使用 --audio-codec none");
    let duration_seconds = options.duration_seconds.or_else(|| media.duration.map(|d| d - options.start_seconds));
    ensure!(duration_seconds.is_none_or(|v| v > 0.0), "开始时间超过媒体总时长");
    let temp = tempfile::tempdir()?;
    let video_path = temp.path().join("video.mjpg");
    let audio_path = temp.path().join(if options.audio_codec == "mp3" { "audio.mp3" } else { "audio.pcm" });
    progress(&Progress {
        stage: "video",
        progress: 0.08,
        message: "转码 MJPEG 视频",
    })?;
    media::transcode(options, &ffmpeg, &input, &video_path, true, duration_seconds)?;
    let mut video = media::scan_video(&video_path, options.fps)?;
    let mut audio = media::Audio {
        packets: Vec::new(),
        rate: 0,
        channels: 0,
        frame_samples: 0,
    };
    if options.audio_codec != "none" {
        progress(&Progress {
            stage: "audio",
            progress: 0.50,
            message: "转码音频",
        })?;
        media::transcode(options, &ffmpeg, &input, &audio_path, false, duration_seconds)?;
        audio = if options.audio_codec == "mp3" {
            media::scan_mp3(&audio_path, options.audio_packet_ms)?
        } else {
            media::scan_pcm(&audio_path, options.audio_rate, options.audio_channels, options.audio_packet_ms)?
        };
    }
    let duration_ms = apply_end_policy(&mut video, &mut audio.packets, &options.end_policy);
    let mut header = Header {
        version: "1.0".into(),
        flags: 0,
        timescale: 1000,
        duration_ms,
        packet_count: 0,
        video_frame_count: 0,
        video_width: options.width,
        video_height: options.height,
        video_fps_num: options.fps.num,
        video_fps_den: options.fps.den,
        audio_codec: match options.audio_codec.as_str() {
            "mp3" => 1,
            "pcm_s16le" => 2,
            _ => 0,
        },
        audio_sample_rate: audio.rate,
        audio_channels: audio.channels,
        audio_bits: if options.audio_codec == "none" { 0 } else { 16 },
        audio_frame_samples: audio.frame_samples,
        max_video_packet: 0,
        max_audio_packet: 0,
        index_offset: 0,
        index_size: 0,
    };
    let output = std::path::absolute(&options.output)?;
    let parent = output.parent().context("输出目录无效")?;
    std::fs::create_dir_all(parent)?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    video.extend(audio.packets);
    progress(&Progress {
        stage: "mux",
        progress: 0.72,
        message: "封装 HZV 音视频包与索引",
    })?;
    format::write(
        staged.as_file_mut(),
        &mut header,
        &mut video,
        &[&video_path, &audio_path],
        options.packet_crc,
        options.loop_hint,
    )?;
    progress(&Progress {
        stage: "verify",
        progress: 0.94,
        message: "深度校验 HZV 文件",
    })?;
    let verified = verify(staged.path(), true)?;
    let mut hash = Sha256::new();
    let mut file = File::open(staged.path())?;
    let mut bytes = [0; 65536];
    loop {
        let n = file.read(&mut bytes)?;
        if n == 0 {
            break;
        }
        hash.update(&bytes[..n]);
    }
    drop(file);
    if options.overwrite {
        staged.persist(&output)?;
    } else {
        staged.persist_noclobber(&output)?;
    }
    let result = ConvertResult {
        output,
        size: verified.size,
        packet_count: header.packet_count,
        video_packets: verified.video_packets,
        audio_packets: verified.audio_packets,
        index_entries: verified.index_entries,
        duration_ms,
        sha256: format!("{:x}", hash.finalize()),
        header,
    };
    progress(&Progress {
        stage: "done",
        progress: 1.0,
        message: "HZV 转换完成",
    })?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_fractional_fps_and_rejects_invalid_values() {
        assert_eq!("30000/1001".parse::<FrameRate>().unwrap(), FrameRate { num: 30000, den: 1001 });
        for value in ["0", "61", "10/0", "-1", "NaN", "1/4294967295"] {
            assert!(value.parse::<FrameRate>().is_err(), "{value}");
        }
    }

    #[test]
    fn shortest_keeps_only_packets_before_cutoff_and_sets_end_flag() {
        let packet = |pts, duration| format::Packet {
            stream: 0,
            flags: 1,
            pts,
            duration,
            offset: 0,
            size: 4,
            sequence: 0,
        };
        let mut video = vec![packet(0, 100), packet(100, 100), packet(200, 100)];
        let mut audio = vec![packet(0, 80), packet(80, 80)];
        assert_eq!(apply_end_policy(&mut video, &mut audio, "shortest"), 160);
        assert_eq!(video.len(), 2);
        assert_ne!(video.last().unwrap().flags & format::END, 0);
        assert_eq!(apply_end_policy(&mut video, &mut audio, "hold-video"), 200);
    }

    #[test]
    fn validation_preserves_existing_output_and_rejects_same_input() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.mp4");
        let output = dir.path().join("output.hzv");
        std::fs::write(&input, b"source").unwrap();
        std::fs::write(&output, b"original").unwrap();
        let mut options = ConvertOptions {
            input: input.clone(),
            output: output.clone(),
            ..Default::default()
        };
        assert!(options.validate().is_err());
        options.overwrite = true;
        assert!(options.validate().is_ok());
        options.output = input;
        assert!(options.validate().is_err());
        assert_eq!(std::fs::read(output).unwrap(), b"original");
    }
}
