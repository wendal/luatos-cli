//! 标准 romfs(`-rom1fs-`, genromfs 兼容)只读镜像生成器。
//!
//! 布局与 LuatOS 核心 `tools/romfs/mkromfs.py` 完全一致(单测与 canonical 产物逐字节对拍),
//! 设备端由 `components/romfs/luat_vfs_romfs.c` 挂载。磁盘结构:
//!
//! ```text
//! head[16] = magic "-rom1fs-" | total_size(be32) | checksum(be32)
//! volume   = 卷名 + NUL, 补齐到 16 字节(本实现卷名为空)
//! entry    = next_and_type(be32) | spec(be32) | size(be32) | checksum(be32)
//!            | name + NUL 补齐到 16 字节 | data 补齐到 16 字节
//! ```
//!
//! `next_and_type` 低 4 位为条目类型(0 硬链接 / 1 目录 / 2 文件 / 3 符号链接), 高 28 位为兄弟链
//! 中下一条目的偏移(0 表示链尾); 目录的 `spec` 是首个子节点偏移; size 仅对文件/符号链接有效。
//! 所有偏移都是镜像内绝对偏移, 全部 16 字节对齐, 校验和为 BE u32 累加(含校验和字段自身时先清零)。
//!
//! 本实现只产出目录与常规文件(资源目录不需要硬链接/符号链接)。

use std::collections::btree_map::Entry;
use std::collections::BTreeMap as Map;

use anyhow::{bail, ensure, Result};

const ROMFS_MAGIC: &[u8; 8] = b"-rom1fs-";
const ROMFS_HEAD_FIXED: usize = 16;
const ROMFS_ENTRY_FIXED: usize = 16;
const ROMFS_ALIGN: usize = 16;
const ROMFS_CKSUM_SPAN: usize = 512;

const T_DIR: u8 = 1;
const T_FILE: u8 = 2;

/// 单级文件名上限(不含 NUL), 与核心 `LUAT_ROMFS_MAX_NAME` 一致。
pub const MAX_NAME: usize = 64;

/// 目录嵌套深度上限(不含根目录)。核心 `LUAT_ROMFS_MAX_DIR_DEPTH` 必须大于该值:
/// 核心的深度检查作用于"目录的每个子节点", 因此宏需为 `MAX_DIR_DEPTH + 1` 才能让
/// 第 `MAX_DIR_DEPTH` 层的目录真正容纳文件。
pub const MAX_DIR_DEPTH: usize = 4;

/// 一个待打入 romfs 的文件: `path` 为 `/` 分隔的相对路径。
pub struct RomfsFile {
    pub path: String,
    pub data: Vec<u8>,
}

/// romfs 镜像与打包过程中跳过的路径。
pub struct RomfsImage {
    pub data: Vec<u8>,
    /// 目录深度超限被跳过的相对路径, 由调用方打印警告。
    pub skipped: Vec<String>,
}

enum Node {
    Dir(Map<String, Node>),
    File(Vec<u8>),
}

struct Ent {
    name: String,
    ty: u8,
    data: Vec<u8>,
    off: usize,
    next_off: usize,
    children: Vec<Ent>,
}

const fn a16(n: usize) -> usize {
    (n + ROMFS_ALIGN - 1) & !(ROMFS_ALIGN - 1)
}

/// 打包成 romfs 镜像。深度超限的路径记入 `skipped` 并丢弃, 其余错误直接返回。
pub fn build(files: &[RomfsFile]) -> Result<RomfsImage> {
    let mut skipped: Vec<String> = Vec::new();
    let mut root: Map<String, Node> = Map::new();

    for f in files {
        let comps: Vec<&str> = f.path.split('/').collect();
        for c in &comps {
            ensure!(!c.is_empty(), "romfs 资源路径含空目录名: {}", f.path);
            ensure!(c.len() <= MAX_NAME, "romfs 单级文件名过长(最大 {MAX_NAME} 字节, 实际 {} 字节): {}", c.len(), f.path);
            ensure!(!c.contains('\0'), "romfs 文件名不允许内嵌 NUL: {}", f.path);
            ensure!(*c != "." && *c != "..", "romfs 文件名不允许 . 或 ..: {}", f.path);
        }
        if comps.len() - 1 > MAX_DIR_DEPTH {
            skipped.push(f.path.clone());
            continue;
        }

        let mut cur = &mut root;
        for c in &comps[..comps.len() - 1] {
            cur = match cur.entry((*c).to_string()).or_insert_with(|| Node::Dir(Map::new())) {
                Node::Dir(m) => m,
                Node::File(_) => bail!("romfs 路径冲突, {} 处既是文件又是目录: {}", c, f.path),
            };
        }
        match cur.entry(comps[comps.len() - 1].to_string()) {
            Entry::Occupied(_) => bail!("romfs 重复路径: {}", f.path),
            Entry::Vacant(v) => {
                v.insert(Node::File(f.data.clone()));
            }
        }
    }

    let mut root_ent = to_ent(String::new(), Node::Dir(root));
    let mut total = ROMFS_HEAD_FIXED + ROMFS_ALIGN; // 头 16 字节 + 空卷名 16 字节
    layout(&mut root_ent, &mut total);
    link(&mut root_ent.children);

    let mut buf = vec![0u8; total];
    buf[..8].copy_from_slice(ROMFS_MAGIC);
    buf[8..12].copy_from_slice(&(total as u32).to_be_bytes());
    put(&root_ent, &mut buf);
    let head = checksum(&buf, 0, total);
    buf[12..16].copy_from_slice(&head.to_be_bytes());

    Ok(RomfsImage { data: buf, skipped })
}

fn to_ent(name: String, node: Node) -> Ent {
    match node {
        Node::File(data) => Ent {
            name,
            ty: T_FILE,
            data,
            off: 0,
            next_off: 0,
            children: Vec::new(),
        },
        Node::Dir(children) => Ent {
            name,
            ty: T_DIR,
            data: Vec::new(),
            off: 0,
            next_off: 0,
            children: children.into_iter().map(|(n, c)| to_ent(n, c)).collect(),
        },
    }
}

/// 前序遍历分配偏移; `*off` 从根目录头之后开始。
fn layout(e: &mut Ent, off: &mut usize) {
    e.off = *off;
    let mut size = ROMFS_ENTRY_FIXED + a16(e.name.len() + 1);
    if e.ty == T_FILE {
        size += a16(e.data.len());
    }
    *off += size;
    for c in e.children.iter_mut() {
        layout(c, off);
    }
}

/// 建立兄弟链: 每条目的 next 指向同目录下一条目, 末项为 0。
fn link(children: &mut [Ent]) {
    let offs: Vec<usize> = children.iter().map(|c| c.off).collect();
    for (i, c) in children.iter_mut().enumerate() {
        c.next_off = offs.get(i + 1).copied().unwrap_or(0);
        link(&mut c.children);
    }
}

fn put(e: &Ent, buf: &mut [u8]) {
    let word0 = ((e.next_off as u32) & 0xFFFF_FFF0) | u32::from(e.ty);
    let spec = if e.ty == T_DIR { e.children.first().map_or(0, |c| c.off as u32) } else { 0 };
    let size = if e.ty == T_FILE { e.data.len() as u32 } else { 0 };
    buf[e.off..e.off + 4].copy_from_slice(&word0.to_be_bytes());
    buf[e.off + 4..e.off + 8].copy_from_slice(&spec.to_be_bytes());
    buf[e.off + 8..e.off + 12].copy_from_slice(&size.to_be_bytes());

    let name_off = e.off + ROMFS_ENTRY_FIXED;
    buf[name_off..name_off + e.name.len()].copy_from_slice(e.name.as_bytes());
    if e.ty == T_FILE {
        let data_off = name_off + a16(e.name.len() + 1);
        buf[data_off..data_off + e.data.len()].copy_from_slice(&e.data);
    }

    let region = ROMFS_ENTRY_FIXED + a16(e.name.len() + 1);
    let ck = checksum(buf, e.off, region);
    buf[e.off + 12..e.off + 16].copy_from_slice(&ck.to_be_bytes());

    for c in &e.children {
        put(c, buf);
    }
}

/// BE u32 累加校验和的补码: 覆盖 `region_len` 与 512 的较小值, 见 `mkromfs.py` 的 `fix_cksum`。
/// 校验和字段本身必须已清零; 读取越过镜像末尾时按 0 补齐。
fn checksum(buf: &[u8], off: usize, region_len: usize) -> u32 {
    let len = region_len.min(ROMFS_CKSUM_SPAN);
    let len = (len + 3) & !3;
    let mut sum: u32 = 0;
    let mut i = 0;
    while i < len {
        let start = off + i;
        let mut word = [0u8; 4];
        for (k, slot) in word.iter_mut().enumerate() {
            if start + k < buf.len() {
                *slot = buf[start + k];
            }
        }
        sum = sum.wrapping_add(u32::from_be_bytes(word));
        i += 4;
    }
    sum.wrapping_neg()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// canonical 产物: 由核心的 mkromfs.py 生成(来源与复现命令见 tests/data/README.md)。
    const GOLDEN: &[u8] = include_bytes!("../tests/data/romfs_fixture.img");

    fn golden_files() -> Vec<RomfsFile> {
        fn f(path: &str, data: &[u8]) -> RomfsFile {
            RomfsFile {
                path: path.to_string(),
                data: data.to_vec(),
            }
        }
        let digits: Vec<u8> = b"0123456789".repeat(7); // 70 字节
        vec![
            f("hello.txt", b"hello romfs\n"),
            f("eighty.txt", &digits),
            f("sub/a.bin", b"AAAA"),
            f("sub/b.bin", b"BBBBBBBB"),
            f("sub/deep/c.dat", b"xyz"),
        ]
    }

    /// 逐字节等于 mkromfs.py 的产物, 保证设备端两种实现看到同一镜像。
    #[test]
    fn matches_canonical_mkromfs_image() {
        let img = build(&golden_files()).unwrap();
        assert!(img.skipped.is_empty());
        assert_eq!(img.data.len(), GOLDEN.len(), "镜像长度应与 canonical 产物一致");
        assert_eq!(img.data, GOLDEN, "镜像内容应与 mkromfs.py 逐字节一致");
    }

    #[test]
    fn head_checksum_self_verifies() {
        let img = build(&golden_files()).unwrap();
        let mut buf = img.data.clone();
        let stored = u32::from_be_bytes(buf[12..16].try_into().unwrap());
        buf[12..16].copy_from_slice(&[0, 0, 0, 0]);
        assert_eq!(checksum(&buf, 0, buf.len()), stored);
    }

    #[test]
    fn layout_is_16_aligned_and_walkable() {
        let img = build(&golden_files()).unwrap();
        let total = u32::from_be_bytes(img.data[8..12].try_into().unwrap()) as usize;
        assert_eq!(total, img.data.len());
        assert_eq!(&img.data[..8], ROMFS_MAGIC);
        // 根目录头位于 32, 类型必须是目录且 spec 指向首个兄弟
        assert_eq!(&img.data[32..36], &[0, 0, 0, T_DIR]);
        let spec = u32::from_be_bytes(img.data[36..40].try_into().unwrap()) as usize;
        assert_eq!(spec % ROMFS_ALIGN, 0);
        assert!(spec > 32 && spec < total);
    }

    #[test]
    fn empty_resource_dir_is_a_valid_image() {
        let img = build(&[]).unwrap();
        // 头 16 + 卷名 16 + 根目录头 32 = 64 字节
        assert_eq!(img.data.len(), 64);
        assert_eq!(&img.data[..8], ROMFS_MAGIC);
        assert_eq!(&img.data[32..36], &[0, 0, 0, T_DIR]);
        assert_eq!(&img.data[36..40], &[0, 0, 0, 0]);
    }

    #[test]
    fn depth_over_limit_is_skipped_not_fatal() {
        let mut files = golden_files();
        files.push(RomfsFile {
            path: "a/b/c/d/keep.bin".into(),
            data: vec![1],
        });
        files.push(RomfsFile {
            path: "a/b/c/d/e/drop.bin".into(),
            data: vec![2],
        });
        files.push(RomfsFile {
            path: "a/b/c/d/e/f/drop2.bin".into(),
            data: vec![3],
        });
        let img = build(&files).unwrap();
        assert_eq!(img.skipped, vec!["a/b/c/d/e/drop.bin".to_string(), "a/b/c/d/e/f/drop2.bin".to_string()]);
    }

    #[test]
    fn name_length_boundary() {
        let ok = RomfsFile {
            path: "x".repeat(MAX_NAME),
            data: vec![],
        };
        assert!(build(&[ok]).is_ok());
        let bad = RomfsFile {
            path: "x".repeat(MAX_NAME + 1),
            data: vec![],
        };
        assert!(build(&[bad]).is_err());
        // UTF-8 按字节计算: 22 个汉字 = 66 字节
        let cjk = RomfsFile {
            path: "汉".repeat(22),
            data: vec![],
        };
        assert!(build(&[cjk]).is_err());
    }

    #[test]
    fn rejects_duplicates_and_file_dir_conflicts() {
        let dup = vec![
            RomfsFile {
                path: "same".into(),
                data: vec![],
            },
            RomfsFile {
                path: "same".into(),
                data: vec![],
            },
        ];
        assert!(build(&dup).is_err());
        let conflict = vec![RomfsFile { path: "n".into(), data: vec![] }, RomfsFile { path: "n/x".into(), data: vec![] }];
        assert!(build(&conflict).is_err());
        for path in ["/abs", "a//b", "a/", "."] {
            let files = vec![RomfsFile {
                path: path.to_string(),
                data: vec![],
            }];
            assert!(build(&files).is_err(), "{path} 应被拒绝");
        }
    }

    #[test]
    fn file_data_is_16_aligned_and_complete() {
        let files = vec![RomfsFile {
            path: "odd.bin".into(),
            data: vec![0x5A; 17],
        }];
        let img = build(&files).unwrap();
        // 头 32 + 根头 32 = 64, 文件条目 16 + a16(9) + a16(17) = 16 + 16 + 32
        assert_eq!(img.data.len(), 64 + 64);
        let off = 64;
        let size = u32::from_be_bytes(img.data[off + 8..off + 12].try_into().unwrap());
        assert_eq!(size, 17);
        // 数据从条目 + 16 + a16(len("odd.bin") + 1) = 条目前后
        let data_off = off + 16 + a16("odd.bin".len() + 1);
        assert_eq!(&img.data[data_off..data_off + 17], &[0x5A; 17]);
        // 尾部对齐填充为 0(镜像初值为 0)
        assert_eq!(&img.data[data_off + 17..data_off + 32], &[0u8; 15]);
    }
}
