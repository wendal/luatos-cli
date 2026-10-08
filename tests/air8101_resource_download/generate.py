#!/usr/bin/env python3
"""Generate disposable resource-download fixtures; no binary assets are committed."""
import argparse
from pathlib import Path
import shutil

CAPACITY = 3 * 1024 * 1024
RAW_LUA = b'this is deliberately not valid Lua!\n\0'
SENTINEL = 'resource-download-lfs-sentinel-v1'

def pattern(size, step, seed):
    cycle = bytes((i * step + seed) % 256 for i in range(256))
    return (cycle * ((size + 255) // 256))[:size]

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
    full = {'profile.txt': b'full\n', 'note.txt': b'arbitrary resource\n', 'palette.bin': pattern(80000, 13, 7),
            'sample.ttf': font, 'source.lua': RAW_LUA, 'x' * 31: b'31-byte-name\n', 'sub/pixels.bin': pattern(3000, 5, 3)}
    generic = {'profile.txt': b'generic\n', 'note.txt': b'arbitrary resource\n', 'palette.bin': pattern(80000, 13, 7),
               'new/image.bin': pattern(3000, 5, 3)}
    sizes = {}
    for name, files in [('full', full), ('generic', generic)]:
        # Global header 24 bytes; each record adds 18 + UTF-8 filename bytes.
        length = CAPACITY - 24 - sum(18 + len(n.encode()) + len(d) for n, d in files.items()) - 18 - len('asset.dat')
        assert length > 65536
        files['asset.dat'] = pattern(length, 37, 11)
        sizes[name] = length
        write_files(out / name, files)
    write_files(out / 'oversized', {'too-big.bin': pattern(CAPACITY, 7, 5)})
    script = out / 'script'
    script.mkdir()
    shutil.copyfile(Path(__file__).with_name('main.lua'), script / 'main.lua')
    (script / 'reference.ttf').write_bytes(font)
    (script / 'manifest.lua').write_text('return {full_asset=%d, generic_asset=%d, font_size=%d}\n' % (sizes['full'], sizes['generic'], len(font)))
    print(out)
    print('full and generic images each pack to exactly', CAPACITY, 'bytes')

if __name__ == '__main__':
    main()
