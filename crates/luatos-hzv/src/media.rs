//! FFmpeg 调用及逐帧扫描，不依赖 Python，也不把整个媒体文件读进内存。

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{bail, ensure, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};

use crate::format::{Packet, END};
use crate::{ConvertOptions, FrameRate};

pub(crate) fn command(binary: &Path) -> Command {
    let mut command = Command::new(binary);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command
}

pub fn find_ffmpeg(explicit: Option<&Path>) -> PathBuf {
    if let Some(path) = explicit {
        return path.to_owned();
    }
    if let Some(path) = std::env::var_os("LUATOS_FFMPEG") {
        return path.into();
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            if let Some(path) = bundled_ffmpeg(dir, std::env::consts::ARCH, std::env::consts::EXE_SUFFIX) {
                return path;
            }
        }
    }
    format!("ffmpeg{}", std::env::consts::EXE_SUFFIX).into()
}

fn bundled_ffmpeg(dir: &Path, arch: &str, suffix: &str) -> Option<PathBuf> {
    let name = format!("ffmpeg{suffix}");
    // macOS universal 发布包保留两份原生程序，由当前运行架构选择。
    [
        dir.join("tools/ffmpeg").join(arch).join("bin").join(&name),
        dir.join("tools/ffmpeg/bin").join(&name),
        dir.join(&name),
    ]
    .into_iter()
    .find(|path| path.is_file())
}

fn run(command: &mut Command) -> Result<Output> {
    command.output().with_context(|| {
        format!(
            "无法启动 {}，请完整解压包含 tools/ffmpeg 的发布包，或用 --ffmpeg / LUATOS_FFMPEG 指定路径",
            command.get_program().to_string_lossy()
        )
    })
}

#[derive(Debug, Serialize)]
pub struct ProbeResult {
    pub path: PathBuf,
    pub size: u64,
    pub duration: Option<f64>,
    pub video: Option<Value>,
    pub audio: Option<Value>,
}

fn parse_duration(text: &str) -> Option<f64> {
    let mut fields = text.trim().split(':');
    let hours = fields.next()?.parse::<f64>().ok()?;
    let minutes = fields.next()?.parse::<f64>().ok()?;
    let seconds = fields.next()?.parse::<f64>().ok()?;
    let duration = hours * 3600.0 + minutes * 60.0 + seconds;
    (duration.is_finite() && duration >= 0.0).then_some(duration)
}

fn parse_probe(text: &str, path: PathBuf, size: u64) -> Result<ProbeResult> {
    let mut result = ProbeResult {
        path,
        size,
        duration: None,
        video: None,
        audio: None,
    };
    for line in text.lines() {
        let line = line.trim();
        if let Some(value) = line.strip_prefix("Duration:") {
            result.duration = parse_duration(value.split(',').next().unwrap_or(""));
        }
        if !line.starts_with("Stream #") {
            continue;
        }
        if let Some((_, value)) = line.split_once("Video:") {
            // attached_pic 是封面，不能当作可转换的视频轨道。
            if result.video.is_some() || line.contains("attached pic") {
                continue;
            }
            let codec = value.trim().split([' ', ',']).next().unwrap_or("");
            let dimensions = value.split(|c: char| c.is_whitespace() || c == ',').find_map(|token| {
                let (w, h) = token.split_once('x')?;
                Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?))
            });
            if let Some((width, height)) = dimensions {
                result.video = Some(json!({"codec_name":codec,"width":width,"height":height}));
            }
        }
        if let Some((_, value)) = line.split_once("Audio:") {
            if result.audio.is_some() {
                continue;
            }
            let codec = value.trim().split([' ', ',']).next().unwrap_or("");
            let words: Vec<_> = value.split_whitespace().collect();
            let sample_rate = words
                .windows(2)
                .find_map(|pair| (pair[1].trim_end_matches(',') == "Hz").then(|| pair[0].parse::<u32>().ok()).flatten());
            let channels = if value.contains("mono") {
                Some(1)
            } else if value.contains("stereo") {
                Some(2)
            } else {
                None
            };
            result.audio = Some(json!({"codec_name":codec,"sample_rate":sample_rate,"channels":channels}));
        }
    }
    ensure!(
        result.video.is_some() || result.audio.is_some(),
        "FFmpeg 未发现可用的音视频轨道：{}",
        text.lines().rev().take(3).collect::<Vec<_>>().join("；")
    );
    Ok(result)
}

/// 使用 FFmpeg 自带探测能力，部署时不额外要求 ffprobe。
pub fn probe(input: &Path, ffmpeg: &Path) -> Result<ProbeResult> {
    let path = input.canonicalize().with_context(|| format!("源文件不存在：{}", input.display()))?;
    ensure!(path.is_file(), "输入必须是文件");
    let output = run(command(ffmpeg).args(["-hide_banner", "-nostdin", "-i"]).arg(&path))?;
    // 仅指定输入、不指定输出时 FFmpeg 正常返回非零，因此以探测内容为准。
    parse_probe(&String::from_utf8_lossy(&output.stderr), path.clone(), path.metadata()?.len())
}

pub(crate) fn transcode(options: &ConvertOptions, ffmpeg: &Path, input: &Path, output: &Path, video: bool, duration: Option<f64>) -> Result<()> {
    let mut cmd = command(ffmpeg);
    cmd.args(["-hide_banner", "-nostdin", "-loglevel", "error", "-y"]);
    if options.start_seconds > 0.0 {
        cmd.args(["-ss", &options.start_seconds.to_string()]);
    }
    cmd.arg("-i").arg(input);
    if let Some(duration) = duration {
        cmd.args(["-t", &duration.to_string()]);
    }
    if video {
        let (w, h) = (options.width, options.height);
        let scale = match options.scale_mode.as_str() {
            "contain" => format!(
                "scale={w}:{h}:force_original_aspect_ratio=decrease:flags=lanczos,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:{}",
                options.background
            ),
            "cover" => format!("scale={w}:{h}:force_original_aspect_ratio=increase:flags=lanczos,crop={w}:{h}"),
            "stretch" => format!("scale={w}:{h}:flags=lanczos"),
            _ => bail!("无效的画面适配方式"),
        };
        cmd.args([
            "-map",
            "0:V:0",
            "-an",
            "-vf",
            &format!("fps={}/{},{scale}", options.fps.num, options.fps.den),
            "-c:v",
            "mjpeg",
            "-q:v",
            &options.video_quality.to_string(),
            "-pix_fmt",
            "yuvj420p",
            "-f",
            "mjpeg",
        ]);
    } else {
        cmd.args(["-map", "0:a:0", "-vn", "-ac", &options.audio_channels.to_string(), "-ar", &options.audio_rate.to_string()]);
        if options.end_policy == "pad-audio" {
            ensure!(duration.is_some(), "pad-audio 需要可探测的时长或 --duration-seconds");
            cmd.args(["-af", "apad"]);
        }
        if options.audio_codec == "mp3" {
            cmd.args([
                "-c:a",
                "libmp3lame",
                "-b:a",
                &format!("{}k", options.audio_bitrate_kbps),
                "-write_xing",
                "0",
                "-id3v2_version",
                "0",
                "-f",
                "mp3",
            ]);
        } else {
            cmd.args(["-c:a", "pcm_s16le", "-f", "s16le"]);
        }
    }
    cmd.arg(output);
    let result = run(&mut cmd)?;
    ensure!(
        result.status.success(),
        "FFmpeg {}转码失败：{}",
        if video { "视频" } else { "音频" },
        String::from_utf8_lossy(&result.stderr).trim()
    );
    Ok(())
}

// 与 Python round 一致采用 ties-to-even，避免分数帧率的累计误差。
fn ticks(samples: u64, rate: u32) -> u64 {
    let n = samples * 1000;
    let (q, r) = (n / rate as u64, n % rate as u64);
    q + u64::from(r * 2 > rate as u64 || (r * 2 == rate as u64 && q % 2 == 1))
}

pub(crate) fn scan_video(path: &Path, fps: FrameRate) -> Result<Vec<Packet>> {
    let reader = BufReader::new(File::open(path)?);
    let mut packets = Vec::new();
    let mut start = None;
    let mut previous = 0;
    for (offset, byte) in reader.bytes().enumerate() {
        let byte = byte?;
        if previous == 255 {
            if byte == 216 && start.is_none() {
                start = Some(offset as u64 - 1);
            } else if byte == 217 {
                if let Some(start_offset) = start.take() {
                    let sequence: u32 = packets.len().try_into()?;
                    let pts = ticks(sequence as u64 * fps.den as u64, fps.num);
                    let next = ticks((sequence as u64 + 1) * fps.den as u64, fps.num);
                    packets.push(Packet {
                        stream: 0,
                        flags: 1,
                        pts,
                        duration: (next - pts).max(1).try_into()?,
                        offset: start_offset,
                        size: (offset as u64 + 1 - start_offset).try_into()?,
                        sequence,
                    });
                }
            }
        }
        previous = byte;
    }
    ensure!(start.is_none(), "MJPEG 包含不完整的 JPEG 帧");
    ensure!(!packets.is_empty(), "转码结果没有 JPEG 帧");
    packets.last_mut().unwrap().flags |= END;
    Ok(packets)
}

#[derive(Debug, PartialEq)]
struct Mp3Frame {
    size: u32,
    samples: u32,
    rate: u32,
    channels: u16,
}

fn mp3_frame(raw: [u8; 4]) -> Option<Mp3Frame> {
    let value = u32::from_be_bytes(raw);
    let version = (value >> 19) & 3;
    let layer = (value >> 17) & 3;
    let bitrate = ((value >> 12) & 15) as usize;
    let sample = ((value >> 10) & 3) as usize;
    if value & 0xffe00000 != 0xffe00000 || version == 1 || layer != 1 || bitrate == 0 || bitrate == 15 || sample == 3 {
        return None;
    }
    let rate = [44100, 48000, 32000][sample]
        / if version == 3 {
            1
        } else if version == 2 {
            2
        } else {
            4
        };
    let kbps = if version == 3 {
        [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 0]
    } else {
        [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160, 0]
    }[bitrate];
    Some(Mp3Frame {
        size: (if version == 3 { 144000 } else { 72000 }) * kbps / rate + ((value >> 9) & 1),
        samples: if version == 3 { 1152 } else { 576 },
        rate,
        channels: if (value >> 6) & 3 == 3 { 1 } else { 2 },
    })
}

pub(crate) struct Audio {
    pub packets: Vec<Packet>,
    pub rate: u32,
    pub channels: u16,
    pub frame_samples: u32,
}

pub(crate) fn scan_mp3(path: &Path, packet_ms: u32) -> Result<Audio> {
    let mut file = BufReader::new(File::open(path)?);
    let length = file.get_ref().metadata()?.len();
    let mut first = [0; 10];
    ensure!(length >= 4, "MP3 转码结果为空或不完整");
    let mut cursor = 0;
    if length >= 10 {
        file.read_exact(&mut first)?;
        if &first[..3] == b"ID3" {
            ensure!(first[6..10].iter().all(|b| b & 128 == 0), "无效的 MP3 ID3 长度");
            cursor = 10 + first[6..10].iter().fold(0u64, |n, b| (n << 7) | *b as u64);
        }
    }
    let mut audio = Audio {
        packets: Vec::new(),
        rate: 0,
        channels: 0,
        frame_samples: 0,
    };
    let (mut total, mut group_samples, mut group_size, mut group_start) = (0u64, 0u64, 0u32, cursor);
    while cursor + 4 <= length {
        file.seek(SeekFrom::Start(cursor))?;
        let mut raw = [0; 4];
        file.read_exact(&mut raw)?;
        let frame = mp3_frame(raw).context("MP3 包含无效的 Layer III 帧")?;
        ensure!(cursor + frame.size as u64 <= length, "MP3 帧被截断");
        if audio.rate == 0 {
            audio.rate = frame.rate;
            audio.channels = frame.channels;
            audio.frame_samples = frame.samples;
        }
        ensure!(audio.rate == frame.rate && audio.channels == frame.channels, "MP3 采样率或声道在流中发生变化");
        if group_size == 0 {
            group_start = cursor;
        }
        group_size = group_size.checked_add(frame.size).context("MP3 音频包过大")?;
        group_samples += frame.samples as u64;
        cursor += frame.size as u64;
        if group_samples * 1000 >= audio.rate as u64 * packet_ms as u64 || cursor == length {
            audio.packets.push(Packet {
                stream: 1,
                flags: 2,
                pts: ticks(total, audio.rate),
                duration: ticks(group_samples, audio.rate).max(1).try_into()?,
                offset: group_start,
                size: group_size,
                sequence: audio.packets.len().try_into()?,
            });
            total += group_samples;
            group_samples = 0;
            group_size = 0;
        }
    }
    ensure!(cursor == length && !audio.packets.is_empty(), "MP3 数据不完整或没有音频帧");
    audio.packets.last_mut().unwrap().flags |= END;
    Ok(audio)
}

pub(crate) fn scan_pcm(path: &Path, rate: u32, channels: u16, packet_ms: u32) -> Result<Audio> {
    let sample_bytes = channels as u64 * 2;
    let size = path.metadata()?.len();
    ensure!(size > 0 && size % sample_bytes == 0, "PCM 音频为空或样本不完整");
    let samples = size / sample_bytes;
    let chunk = (rate as u64 * packet_ms as u64 / 1000).max(1);
    let mut audio = Audio {
        packets: Vec::new(),
        rate,
        channels,
        frame_samples: 0,
    };
    let mut cursor = 0;
    while cursor < samples {
        let count = chunk.min(samples - cursor);
        audio.packets.push(Packet {
            stream: 1,
            flags: 2,
            pts: ticks(cursor, rate),
            duration: ticks(count, rate).max(1).try_into()?,
            offset: cursor * sample_bytes,
            size: (count * sample_bytes).try_into()?,
            sequence: audio.packets.len().try_into()?,
        });
        cursor += count;
    }
    audio.packets.last_mut().unwrap().flags |= END;
    Ok(audio)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_bundled_ffmpeg_after_relocation_and_selects_native_architecture() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        assert!(bundled_ffmpeg(root, "aarch64", "").is_none());
        let direct = root.join("ffmpeg");
        std::fs::write(&direct, b"compat").unwrap();
        let native = root.join("tools/ffmpeg/bin/ffmpeg");
        std::fs::create_dir_all(native.parent().unwrap()).unwrap();
        std::fs::write(&native, b"native").unwrap();
        assert_eq!(bundled_ffmpeg(root, "aarch64", ""), Some(native.clone()));
        let arm = root.join("tools/ffmpeg/aarch64/bin/ffmpeg");
        std::fs::create_dir_all(arm.parent().unwrap()).unwrap();
        std::fs::write(&arm, b"arm").unwrap();
        assert_eq!(bundled_ffmpeg(root, "aarch64", ""), Some(arm));
        assert_eq!(bundled_ffmpeg(root, "x86_64", ""), Some(native));
        let windows = root.join("tools/ffmpeg/bin/ffmpeg.exe");
        std::fs::write(&windows, b"windows").unwrap();
        assert_eq!(bundled_ffmpeg(root, "x86_64", ".exe"), Some(windows));
    }

    #[test]
    fn probe_handles_video_audio_and_unknown_duration() {
        let p = parse_probe(
            "Duration: 00:01:02.50, start: 0\n  Stream #0:0: Video: h264 (High), yuv420p, 1920x1080 [SAR 1:1], 30 fps\n  Stream #0:1: Audio: aac (LC), 48000 Hz, stereo, fltp",
            "input.mp4".into(),
            42,
        )
        .unwrap();
        assert_eq!(p.duration, Some(62.5));
        assert_eq!(p.video.unwrap()["width"], 1920);
        assert_eq!(p.audio.unwrap()["sample_rate"], 48000);
        assert!(parse_probe("bad input", "bad".into(), 0).is_err());
    }

    #[test]
    fn mp3_headers_cover_mpeg_versions_and_invalid_frames() {
        assert_eq!(
            mp3_frame([0xff, 0xfb, 0x58, 0xc0]),
            Some(Mp3Frame {
                size: 288,
                samples: 1152,
                rate: 32000,
                channels: 1
            })
        );
        assert_eq!(
            mp3_frame([0xff, 0xf3, 0x48, 0xc0]),
            Some(Mp3Frame {
                size: 144,
                samples: 576,
                rate: 16000,
                channels: 1
            })
        );
        assert!(mp3_frame([0; 4]).is_none());
        assert!(mp3_frame([0xff, 0xfb, 0x0c, 0]).is_none());
    }

    #[test]
    fn scanners_preserve_frames_and_fractional_timing() {
        let dir = tempfile::tempdir().unwrap();
        let video = dir.path().join("video.mjpg");
        std::fs::write(&video, [255, 216, 1, 255, 217, 255, 216, 2, 255, 217, 255, 216, 3, 255, 217]).unwrap();
        let frames = scan_video(&video, FrameRate { num: 24, den: 1 }).unwrap();
        assert_eq!(frames.iter().map(|p| (p.pts, p.duration)).collect::<Vec<_>>(), [(0, 42), (42, 41), (83, 42)]);
        assert_eq!(frames[1].offset, 5);
        let mp3 = dir.path().join("audio.mp3");
        let mut bytes = Vec::new();
        for _ in 0..4 {
            bytes.extend_from_slice(&[0xff, 0xfb, 0x58, 0xc0]);
            bytes.extend_from_slice(&[0; 284]);
        }
        std::fs::write(&mp3, bytes).unwrap();
        let audio = scan_mp3(&mp3, 80).unwrap();
        assert_eq!(audio.packets.len(), 2);
        assert_eq!((audio.packets[0].size, audio.packets[1].pts), (864, 108));
        std::fs::write(&video, [255, 216, 1]).unwrap();
        assert!(scan_video(&video, FrameRate::default()).is_err());
        std::fs::write(&mp3, [0xff, 0xfb, 0x58, 0xc0]).unwrap();
        assert!(scan_mp3(&mp3, 80).is_err());
    }
}
