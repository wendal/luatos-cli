#!/usr/bin/env python3
"""Generate disposable resource-download fixtures; no binary assets are committed.

资源分区格式 = 标准 romfs(`-rom1fs-`)按 BK CRC 每 32 字节数据 + 2 字节 CRC 帧化。
本脚本用 Python 复算 crates/luatos-luadb/src/romfs.rs 的布局, 让 `full` / `generic`
两套资源的干净镜像正好填满 1MiB 净载荷, 帧化后正好是 1088KiB 资源分区容量。
"""
import argparse
from pathlib import Path
import shutil

# 实机临时布局: luat_res,0x6D7000,1088k
CAPACITY = 1088 * 1024                # 1_114_112 资源分区字节数
NET_PAYLOAD = CAPACITY // 34 * 32     # 1_048_576 帧化后能装入的干净 romfs 镜像上限
MAX_NAME = 64                         # crates/luatos-luadb/src/romfs.rs: MAX_NAME, 单级名字节数
MAX_DIR_DEPTH = 4                     # crates/luatos-luadb/src/romfs.rs: MAX_DIR_DEPTH, 目录层数
RAW_LUA = b'this is deliberately not valid Lua!\n\0'
SENTINEL = 'resource-download-lfs-sentinel-v1'


def align16(n):
    return (n + 15) & ~15


def pattern(size, step, seed):
    cycle = bytes((i * step + seed) % 256 for i in range(256))
    return (cycle * ((size + 255) // 256))[:size]


def romfs_size(files):
    """干净 romfs 镜像长度; 公式与 crates/luatos-luadb/src/romfs.rs 的 layout() 完全一致:

    头 16 + 卷名 16
    根目录条目   = 16 + align16(0 + 1)
    文件条目     = 16 + align16(名字字节数 + 1) + align16(数据字节数)
    中间目录条目 = 16 + align16(目录名字节数 + 1)

    `files` 为 {相对路径: 字节数}, 路径用 '/' 分隔, 与 CLI 收集资源目录的规则相同。
    第 MAX_DIR_DEPTH 层之后的文件被 CLI 告警跳过, 不进入镜像, 因此也不计开销;
    单级名超过 MAX_NAME 字节时 romfs 编码器直接报错, 这里同样抛出 ValueError。
    """
    total = 32 + 16 + align16(1)
    dirs = set()
    for path, size in files.items():
        parts = path.split('/')
        for part in parts:
            if len(part.encode()) > MAX_NAME:
                raise ValueError('romfs 单级名超过 %d 字节: %s' % (MAX_NAME, path))
        if len(parts) - 1 > MAX_DIR_DEPTH:
            continue
        for i in range(len(parts) - 1):
            dirs.add('/'.join(parts[:i + 1]))
        total += 16 + align16(len(parts[-1].encode()) + 1) + align16(size)
    return total + sum(16 + align16(len(d.split('/')[-1].encode()) + 1) for d in dirs)


def framed_size(clean_size):
    """BK CRC 帧化长度: 每 32 字节数据 + 2 字节 CRC, 末帧以 0xFF 补齐(luatos-luadb/src/lib.rs)。"""
    return (clean_size + 31) // 32 * 34


def write_files(root, files):
    root.mkdir(parents=True, exist_ok=False)
    for name, data in files.items():
        p = root / name
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_bytes(data)


def main():
    p = argparse.ArgumentParser()
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--font', type=Path, required=True, help='Small real TTF/OTF, copied without modification')
    args = p.parse_args()
    out = args.output
    if out.exists():
        p.error('output must not exist; keep previous evidence intact')
    font = args.font.read_bytes()
    if len(font) > 300000:
        p.error('use a font smaller than 300000 bytes to fit script reference partition')

    # 只有 64 字节及以下的名字能进镜像; 65 字节名不落盘, 只校验上限本身
    try:
        romfs_size({'x' * (MAX_NAME + 1): 0})
    except ValueError:
        pass
    else:
        raise SystemExit('romfs_size 未拒绝 %d 字节的单级名' % (MAX_NAME + 1))
    # 第 4 层目录保留, 第 5 层被跳过(镜像里连它的目录都不会出现)
    assert romfs_size({'a/b/c/d/keep.bin': 8}) == 64 + 4 * (16 + 16) + (16 + 16 + 16)
    assert romfs_size({'a/b/c/d/e/x.bin': 8}) == 64

    full = {'profile.txt': b'full\n', 'note.txt': b'arbitrary resource\n', 'palette.bin': pattern(80000, 13, 7),
            'sample.ttf': font, 'source.lua': RAW_LUA, 'x' * MAX_NAME: b'64-byte-name\n',
            'sub/pixels.bin': pattern(3000, 5, 3)}
    generic = {'profile.txt': b'generic\n', 'note.txt': b'arbitrary resource\n', 'palette.bin': pattern(80000, 13, 7),
               'new/image.bin': pattern(3000, 5, 3)}
    # deep 资源根下 a/b/c/d 是第 4 层(保留), a/b/c/d/e 是第 5 层(CLI 告警并跳过)
    deep = {'profile.txt': b'deep\n', 'a/b/c/d/keep.bin': b'4-level-kept\n', 'a/b/c/d/e/x.bin': pattern(512, 3, 9)}

    sizes = {}
    for name, files in [('full', full), ('generic', generic)]:
        base = {k: len(v) for k, v in files.items()}
        # asset.dat 把干净镜像补齐到正好 NET_PAYLOAD 字节(目标与已有开销都是 16 的倍数)
        length = NET_PAYLOAD - romfs_size(base) - (16 + align16(len('asset.dat') + 1))
        assert length > 65536 and length % 16 == 0
        files['asset.dat'] = pattern(length, 37, 11)
        clean = romfs_size({k: len(v) for k, v in files.items()})
        assert clean == NET_PAYLOAD, '%s 干净镜像 %d != %d' % (name, clean, NET_PAYLOAD)
        assert framed_size(clean) == CAPACITY, '%s 帧化后 %d != %d' % (name, framed_size(clean), CAPACITY)
        sizes[name] = length
        write_files(out / name, files)

    # 单文件数据就占满净载荷, 加上 romfs 条目开销必然超容量 -> CLI 必须在连串口前拒绝
    oversized = {'too-big.bin': pattern(NET_PAYLOAD, 7, 5)}
    assert romfs_size({k: len(v) for k, v in oversized.items()}) > NET_PAYLOAD
    write_files(out / 'oversized', oversized)

    write_files(out / 'deep', deep)
    deep_image = romfs_size({k: len(v) for k, v in deep.items()})
    # 显式算式: 头+卷名+根目录 64 + 4 层单字目录 4*(16+16) + profile.txt(16+16+16) + keep.bin(16+16+16)
    assert deep_image == 64 + 4 * (16 + 16) + (16 + 16 + 16) + (16 + 16 + 16)
    assert framed_size(deep_image) < CAPACITY

    script = out / 'script'
    script.mkdir()
    shutil.copyfile(Path(__file__).with_name('main.lua'), script / 'main.lua')
    (script / 'reference.ttf').write_bytes(font)
    (script / 'manifest.lua').write_text(
        'return {full_asset=%d, generic_asset=%d, font_size=%d, full_image=%d, generic_image=%d, deep_keep=%d, deep_image=%d}\n'
        % (sizes['full'], sizes['generic'], len(font), NET_PAYLOAD, NET_PAYLOAD, len(deep['a/b/c/d/keep.bin']), deep_image))

    print(out)
    for name, files in [('full', full), ('generic', generic), ('deep', deep)]:
        clean = romfs_size({k: len(v) for k, v in files.items()})
        print('%s: clean romfs %d bytes, BK CRC framed %d bytes' % (name, clean, framed_size(clean)))
        for path in sorted(files):
            mark = ' (skipped: %d dirs, over depth limit)' % path.count('/') if path.count('/') > MAX_DIR_DEPTH else ''
            print('  %-70s %8d%s' % (path, len(files[path]), mark))
    print('full and generic clean romfs images are exactly %d bytes;' % NET_PAYLOAD,
          'framed with BK CRC they are exactly %d bytes = the 1088KiB partition capacity' % framed_size(NET_PAYLOAD))


if __name__ == '__main__':
    main()
