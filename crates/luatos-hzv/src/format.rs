//! 与 AirMaster HZV v1 兼容的小端容器，媒体数据按文件偏移读取。

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use anyhow::{ensure, Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};

const HEADER_SIZE: u64 = 128;
pub(crate) const END: u8 = 8;

#[derive(Clone, Debug)]
pub(crate) struct Packet {
    pub stream: u8,
    pub flags: u8,
    pub pts: u64,
    pub duration: u32,
    pub offset: u64,
    pub size: u32,
    pub sequence: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct Header {
    pub version: String,
    pub flags: u32,
    pub timescale: u32,
    pub duration_ms: u64,
    pub packet_count: u32,
    pub video_frame_count: u32,
    pub video_width: u16,
    pub video_height: u16,
    pub video_fps_num: u32,
    pub video_fps_den: u32,
    pub audio_codec: u32,
    pub audio_sample_rate: u32,
    pub audio_channels: u16,
    pub audio_bits: u16,
    pub audio_frame_samples: u32,
    pub max_video_packet: u32,
    pub max_audio_packet: u32,
    pub index_offset: u64,
    pub index_size: u64,
}

#[derive(Debug, Serialize)]
pub struct Verification {
    pub ok: bool,
    pub size: u64,
    pub header: Header,
    pub video_packets: u32,
    pub audio_packets: u32,
    pub index_entries: u32,
    pub deep: bool,
}

pub(crate) fn put(dst: &mut [u8], offset: usize, bytes: &[u8]) {
    dst[offset..offset + bytes.len()].copy_from_slice(bytes);
}
fn u16_at(b: &[u8], n: usize) -> u16 {
    u16::from_le_bytes(b[n..n + 2].try_into().unwrap())
}
fn u32_at(b: &[u8], n: usize) -> u32 {
    u32::from_le_bytes(b[n..n + 4].try_into().unwrap())
}
fn u64_at(b: &[u8], n: usize) -> u64 {
    u64::from_le_bytes(b[n..n + 8].try_into().unwrap())
}
fn read_at<const N: usize>(file: &mut File, offset: u64) -> Result<[u8; N]> {
    let mut bytes = [0; N];
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(&mut bytes).context("HZV 文件被截断")?;
    Ok(bytes)
}
fn check_crc(bytes: &[u8], offset: usize) -> Result<()> {
    let expected = u32_at(bytes, offset);
    let mut check = bytes.to_vec();
    put(&mut check, offset, &[0; 4]);
    ensure!(crc32fast::hash(&check) == expected, "HZV CRC32 校验失败");
    Ok(())
}

pub(crate) fn write(file: &mut File, header: &mut Header, packets: &mut [Packet], sources: &[&Path; 2], packet_crc: bool, loop_hint: bool) -> Result<()> {
    packets.sort_by_key(|p| (p.pts, if p.stream == 1 { 0 } else { 1 }, p.sequence));
    ensure!(!packets.is_empty(), "不能生成空 HZV 文件");
    header.packet_count = packets.len().try_into()?;
    header.video_frame_count = packets.iter().filter(|p| p.stream == 0).count().try_into()?;
    header.max_video_packet = packets.iter().filter(|p| p.stream == 0).map(|p| p.size).max().unwrap_or(0);
    header.max_audio_packet = packets.iter().filter(|p| p.stream == 1).map(|p| p.size).max().unwrap_or(0);
    header.flags = 1 | 4 | 16 | 32 | if header.audio_codec != 0 { 2 } else { 0 } | if packet_crc { 8 } else { 0 } | if loop_hint { 64 } else { 0 };
    let mut video = File::open(sources[0])?;
    let mut audio = if header.audio_codec != 0 { Some(File::open(sources[1])?) } else { None };
    file.write_all(&[0; 128])?;
    let mut index = Vec::new();
    let mut next_audio_index = 0;
    let mut buffer = [0u8; 65536];
    for packet in packets {
        let offset = file.stream_position()?;
        let source = if packet.stream == 0 {
            &mut video
        } else {
            audio.as_mut().context("缺少音频数据")?
        };
        source.seek(SeekFrom::Start(packet.offset))?;
        // 先占位包头，再分块复制媒体数据，同时计算 CRC，内存不随媒体大小增长。
        file.write_all(&[0; 32])?;
        let mut remaining = packet.size as usize;
        let mut crc = crc32fast::Hasher::new();
        while remaining > 0 {
            let count = remaining.min(buffer.len());
            source.read_exact(&mut buffer[..count])?;
            if packet_crc {
                crc.update(&buffer[..count]);
            }
            file.write_all(&buffer[..count])?;
            remaining -= count;
        }
        file.write_all(&[0; 3][..((4 - packet.size % 4) % 4) as usize])?;
        let end = file.stream_position()?;
        let mut raw = [0; 32];
        put(&mut raw, 0, b"HZPK");
        put(&mut raw, 4, &32u16.to_le_bytes());
        raw[6] = packet.stream;
        raw[7] = packet.flags;
        put(&mut raw, 8, &packet.pts.to_le_bytes());
        put(&mut raw, 16, &packet.duration.to_le_bytes());
        put(&mut raw, 20, &packet.size.to_le_bytes());
        put(&mut raw, 24, &if packet_crc { crc.finalize() } else { 0 }.to_le_bytes());
        put(&mut raw, 28, &packet.sequence.to_le_bytes());
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(&raw)?;
        file.seek(SeekFrom::Start(end))?;
        if packet.stream == 0 || packet.pts >= next_audio_index {
            let mut entry = [0; 24];
            put(&mut entry, 0, &packet.pts.to_le_bytes());
            put(&mut entry, 8, &offset.to_le_bytes());
            put(&mut entry, 16, &packet.size.to_le_bytes());
            put(&mut entry, 20, &(packet.flags as u16).to_le_bytes());
            entry[22] = packet.stream;
            index.extend_from_slice(&entry);
            if packet.stream == 1 {
                next_audio_index = packet.pts + 1000;
            }
        }
    }
    header.index_offset = file.stream_position()?;
    header.index_size = 32 + index.len() as u64;
    let mut raw = [0; 32];
    put(&mut raw, 0, b"HZIX");
    put(&mut raw, 4, &1u16.to_le_bytes());
    put(&mut raw, 6, &24u16.to_le_bytes());
    put(&mut raw, 8, &u32::try_from(index.len() / 24)?.to_le_bytes());
    put(&mut raw, 16, &crc32fast::hash(&index).to_le_bytes());
    file.write_all(&raw)?;
    file.write_all(&index)?;
    raw = [0; 32];
    put(&mut raw, 0, b"HZTL");
    put(&mut raw, 4, &1u16.to_le_bytes());
    put(&mut raw, 8, &header.index_offset.to_le_bytes());
    put(&mut raw, 16, &header.index_size.to_le_bytes());
    let crc = crc32fast::hash(&raw);
    put(&mut raw, 24, &crc.to_le_bytes());
    file.write_all(&raw)?;
    let mut raw = [0; 128];
    put(&mut raw, 0, b"HZV1");
    put(&mut raw, 4, &1u16.to_le_bytes());
    put(&mut raw, 8, &128u32.to_le_bytes());
    put(&mut raw, 12, &header.flags.to_le_bytes());
    put(&mut raw, 16, &1000u32.to_le_bytes());
    put(&mut raw, 20, &(1u32 + u32::from(header.audio_codec != 0)).to_le_bytes());
    put(&mut raw, 24, &header.duration_ms.to_le_bytes());
    put(&mut raw, 32, &HEADER_SIZE.to_le_bytes());
    put(&mut raw, 40, &header.index_offset.to_le_bytes());
    put(&mut raw, 48, &header.index_size.to_le_bytes());
    put(&mut raw, 56, &header.packet_count.to_le_bytes());
    put(&mut raw, 60, &header.video_frame_count.to_le_bytes());
    put(&mut raw, 64, &1u32.to_le_bytes());
    put(&mut raw, 68, &header.video_width.to_le_bytes());
    put(&mut raw, 70, &header.video_height.to_le_bytes());
    put(&mut raw, 72, &header.video_fps_num.to_le_bytes());
    put(&mut raw, 76, &header.video_fps_den.to_le_bytes());
    put(&mut raw, 80, &1u32.to_le_bytes()); // 播放端输出 RGB565；负载仍为 JPEG。
    put(&mut raw, 84, &header.audio_codec.to_le_bytes());
    put(&mut raw, 88, &header.audio_sample_rate.to_le_bytes());
    put(&mut raw, 92, &header.audio_channels.to_le_bytes());
    put(&mut raw, 94, &header.audio_bits.to_le_bytes());
    put(&mut raw, 96, &header.audio_frame_samples.to_le_bytes());
    put(&mut raw, 100, &header.max_video_packet.to_le_bytes());
    put(&mut raw, 104, &header.max_audio_packet.to_le_bytes());
    let id = Sha256::digest(format!("{:?}-{:?}", sources, std::time::SystemTime::now()).as_bytes());
    put(&mut raw, 112, &id[..16]);
    let crc = crc32fast::hash(&raw);
    put(&mut raw, 108, &crc.to_le_bytes());
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&raw)?;
    file.sync_all()?;
    Ok(())
}

/// 校验容器结构、索引与时间戳；deep 额外检查负载 CRC 与 JPEG 边界。
pub fn verify(path: &Path, deep: bool) -> Result<Verification> {
    let mut file = File::open(path).with_context(|| format!("无法打开 {}", path.display()))?;
    let size = file.metadata()?.len();
    let raw = read_at::<128>(&mut file, 0)?;
    ensure!(&raw[..4] == b"HZV1" && u16_at(&raw, 4) == 1 && u32_at(&raw, 8) == 128, "无效的 HZV v1 文件头");
    check_crc(&raw, 108)?;
    let index_offset = u64_at(&raw, 40);
    let index_size = u64_at(&raw, 48);
    ensure!(index_offset >= HEADER_SIZE && index_size >= 32, "HZV 索引区域无效");
    ensure!(index_offset.checked_add(index_size).and_then(|v| v.checked_add(32)) == Some(size), "HZV 索引或文件尾越界");
    let header = Header {
        version: format!("{}.{}", u16_at(&raw, 4), u16_at(&raw, 6)),
        flags: u32_at(&raw, 12),
        timescale: u32_at(&raw, 16),
        duration_ms: u64_at(&raw, 24),
        packet_count: u32_at(&raw, 56),
        video_frame_count: u32_at(&raw, 60),
        video_width: u16_at(&raw, 68),
        video_height: u16_at(&raw, 70),
        video_fps_num: u32_at(&raw, 72),
        video_fps_den: u32_at(&raw, 76),
        audio_codec: u32_at(&raw, 84),
        audio_sample_rate: u32_at(&raw, 88),
        audio_channels: u16_at(&raw, 92),
        audio_bits: u16_at(&raw, 94),
        audio_frame_samples: u32_at(&raw, 96),
        max_video_packet: u32_at(&raw, 100),
        max_audio_packet: u32_at(&raw, 104),
        index_offset,
        index_size,
    };
    ensure!(header.timescale > 0 && header.video_fps_num > 0 && header.video_fps_den > 0, "HZV 时间基或帧率无效");
    ensure!(u64_at(&raw, 32) == 128 && u32_at(&raw, 64) == 1 && header.audio_codec <= 2, "HZV 数据偏移或编码不受支持");
    ensure!(header.flags & 5 == 5 && header.video_width > 0 && header.video_height > 0, "HZV 视频或索引标志无效");
    ensure!(u32_at(&raw, 20) == 1 + u32::from(header.audio_codec != 0), "HZV 流数量不匹配");
    if header.audio_codec != 0 {
        ensure!(header.audio_sample_rate > 0 && (1..=2).contains(&header.audio_channels), "HZV 音频参数无效");
    }
    let mut offset = HEADER_SIZE;
    let mut counts = [0u32; 2];
    let mut previous = [None; 2];
    let mut records = BTreeMap::new();
    let mut buffer = [0; 65536];
    for _ in 0..header.packet_count {
        ensure!(offset + 32 <= index_offset, "HZV 包头越界");
        let p = read_at::<32>(&mut file, offset)?;
        ensure!(&p[..4] == b"HZPK" && u16_at(&p, 4) == 32 && p[6] <= 1, "HZV 媒体包头无效");
        let stream = p[6] as usize;
        let pts = u64_at(&p, 8);
        let payload_size = u32_at(&p, 20);
        ensure!(payload_size > 0 && u32_at(&p, 16) > 0, "HZV 空媒体包或包时长无效");
        let next = offset + 32 + (payload_size as u64).div_ceil(4) * 4;
        ensure!(next <= index_offset, "HZV 媒体负载越界");
        ensure!(previous[stream].is_none_or(|v| pts >= v), "HZV PTS 时间戳倒退");
        previous[stream] = Some(pts);
        counts[stream] += 1;
        records.insert(offset, (pts, payload_size, p[7] as u16, p[6]));
        if deep {
            let mut crc = crc32fast::Hasher::new();
            let mut remaining = payload_size as usize;
            while remaining > 0 {
                let count = remaining.min(buffer.len());
                file.read_exact(&mut buffer[..count])?;
                crc.update(&buffer[..count]);
                remaining -= count;
            }
            ensure!(header.flags & 8 == 0 || crc.finalize() == u32_at(&p, 24), "HZV 媒体包 CRC32 校验失败，偏移 {offset}");
            if stream == 0 {
                ensure!(payload_size >= 4, "HZV JPEG 帧过短");
                ensure!(
                    read_at::<2>(&mut file, offset + 32)? == [255, 216] && read_at::<2>(&mut file, offset + 30 + payload_size as u64)? == [255, 217],
                    "HZV JPEG 帧不完整"
                );
            }
        }
        offset = next;
    }
    ensure!(
        offset == index_offset && counts[0] == header.video_frame_count && counts[0] > 0,
        "HZV 媒体包数量或结束位置不匹配"
    );
    ensure!(
        (counts[1] > 0) == (header.audio_codec != 0) && (header.flags & 2 != 0) == (header.audio_codec != 0),
        "HZV 音频包与编码信息不匹配"
    );
    let ix = read_at::<32>(&mut file, index_offset)?;
    let entry_count = u32_at(&ix, 8);
    ensure!(&ix[..4] == b"HZIX" && u16_at(&ix, 4) == 1 && u16_at(&ix, 6) == 24, "HZV 索引头无效");
    ensure!(32 + entry_count as u64 * 24 == index_size, "HZV 索引长度不匹配");
    let mut crc = crc32fast::Hasher::new();
    let mut indexed_video = std::collections::BTreeSet::new();
    for _ in 0..entry_count {
        let mut entry = [0; 24];
        file.read_exact(&mut entry)?;
        crc.update(&entry);
        let packet_offset = u64_at(&entry, 8);
        let expected = (u64_at(&entry, 0), u32_at(&entry, 16), u16_at(&entry, 20), entry[22]);
        ensure!(records.get(&packet_offset) == Some(&expected), "HZV 索引项与媒体包不匹配");
        if entry[22] == 0 {
            ensure!(indexed_video.insert(packet_offset), "HZV 视频索引重复");
        }
    }
    ensure!(
        crc.finalize() == u32_at(&ix, 16) && indexed_video.len() == counts[0] as usize,
        "HZV 索引 CRC32 错误或视频索引不完整"
    );
    let trailer = read_at::<32>(&mut file, index_offset + index_size)?;
    ensure!(&trailer[..4] == b"HZTL" && u16_at(&trailer, 4) == 1, "HZV 文件尾无效");
    ensure!(u64_at(&trailer, 8) == index_offset && u64_at(&trailer, 16) == index_size, "HZV 文件尾索引位置不匹配");
    check_crc(&trailer, 24)?;
    Ok(Verification {
        ok: true,
        size,
        header,
        video_packets: counts[0],
        audio_packets: counts[1],
        index_entries: entry_count,
        deep,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // 由 AirMaster tools/hzv/hzv_converter.py 生成，防止读写两端同时写错而自测通过。
    const AIRMASTER: &[u8] = include_bytes!("testdata/airmaster-v1.hzv");

    #[test]
    fn reads_airmaster_fixture_and_writes_identical_packet_and_index_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("airmaster.hzv");
        std::fs::write(&source, AIRMASTER).unwrap();
        let checked = verify(&source, true).unwrap();
        assert_eq!(checked.video_packets, 10);
        assert_eq!((checked.header.video_width, checked.header.video_height), (32, 24));
        assert_eq!(checked.header.audio_codec, 1);
        let mut packets = Vec::new();
        let mut offset = 128usize;
        while offset < checked.header.index_offset as usize {
            let p = &AIRMASTER[offset..offset + 32];
            let size = u32_at(p, 20);
            packets.push(Packet {
                stream: p[6],
                flags: p[7],
                pts: u64_at(p, 8),
                duration: u32_at(p, 16),
                size,
                offset: offset as u64 + 32,
                sequence: u32_at(p, 28),
            });
            offset += 32 + (size as usize).div_ceil(4) * 4;
        }
        let output = dir.path().join("rust.hzv");
        let mut file = File::create(&output).unwrap();
        let mut header = checked.header;
        write(&mut file, &mut header, &mut packets, &[&source, &source], true, false).unwrap();
        drop(file);
        verify(&output, true).unwrap();
        let generated = std::fs::read(output).unwrap();
        // 内容 ID 及其影响的文件头 CRC 不要求一致，其余字段和媒体区域必须逐字节一致。
        assert_eq!(&generated[..108], &AIRMASTER[..108]);
        assert_eq!(&generated[128..], &AIRMASTER[128..]);
    }

    #[test]
    fn rejects_corruption_in_header_payload_index_and_trailer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.hzv");
        let index = u64_at(AIRMASTER, 40) as usize;
        for offset in [108, 160, index + 16, AIRMASTER.len() - 8] {
            let mut bytes = AIRMASTER.to_vec();
            bytes[offset] ^= 1;
            std::fs::write(&path, bytes).unwrap();
            assert!(verify(&path, true).is_err(), "损坏偏移 {offset}");
            if offset == 160 {
                assert!(verify(&path, false).is_ok());
            }
        }
        for length in [0, 127, 160, AIRMASTER.len() - 1] {
            std::fs::write(&path, &AIRMASTER[..length]).unwrap();
            assert!(verify(&path, true).is_err());
        }
        // 改坏索引对应的 PTS，并更新索引 CRC，验证不会只检查 CRC。
        let mut bytes = AIRMASTER.to_vec();
        bytes[index + 32] ^= 1;
        let crc = crc32fast::hash(&bytes[index + 32..bytes.len() - 32]);
        put(&mut bytes, index + 16, &crc.to_le_bytes());
        std::fs::write(&path, bytes).unwrap();
        assert!(verify(&path, true).unwrap_err().to_string().contains("索引项"));
        // 构造有合法文件头 CRC 的越界偏移，不能溢出或按恶意长度分配内存。
        let mut bytes = AIRMASTER.to_vec();
        put(&mut bytes, 40, &u64::MAX.to_le_bytes());
        put(&mut bytes, 108, &[0; 4]);
        let crc = crc32fast::hash(&bytes[..128]);
        put(&mut bytes, 108, &crc.to_le_bytes());
        std::fs::write(&path, bytes).unwrap();
        assert!(verify(&path, true).is_err());
    }
}
