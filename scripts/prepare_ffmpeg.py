#!/usr/bin/env python3
"""构建期准备固定版本的 FFmpeg；运行 luatos-cli 时不需要 Python 或联网。"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import tempfile
import urllib.request
import zipfile


ROOT = Path(__file__).resolve().parents[1]


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def download(asset, cache):
    path = cache / asset["sha256"]
    if not path.is_file() or digest(path) != asset["sha256"]:
        request = urllib.request.Request(asset["url"], headers={"User-Agent": "luatos-cli-ffmpeg-packaging"})
        with tempfile.NamedTemporaryFile(dir=cache, delete=False) as stream:
            temporary = Path(stream.name)
            try:
                with urllib.request.urlopen(request, timeout=120) as response:
                    shutil.copyfileobj(response, stream)
                stream.close()
                if digest(temporary) != asset["sha256"]:
                    raise RuntimeError(f"FFmpeg 下载校验失败：{asset['url']}")
                os.replace(temporary, path)
            finally:
                temporary.unlink(missing_ok=True)
    return path


def prepare(target, cache):
    manifest = json.loads((ROOT / "vendor/ffmpeg/manifest.json").read_text(encoding="utf-8"))
    if target not in manifest:
        raise RuntimeError(f"未配置 FFmpeg 目标平台：{target}")
    spec = manifest[target]
    destination = ROOT / "vendor/ffmpeg" / target
    binary = destination / ("bin/ffmpeg.exe" if "windows" in target else "bin/ffmpeg")
    # 已纳入 Git LFS 的 Windows x64 资源无需下载。
    if spec.get("vendored"):
        if not binary.is_file() or digest(binary) != spec["binary_sha256"]:
            raise RuntimeError("Windows x64 FFmpeg 缺失或损坏，请执行 git lfs pull --include='vendor/ffmpeg/**'")
        for name, expected in spec["notices"].items():
            if digest(destination / name) != expected:
                raise RuntimeError(f"FFmpeg 随包声明校验失败：{name}")
        print(f"FFmpeg 已就绪：{target}")
        return
    cache.mkdir(parents=True, exist_ok=True)
    # 只写清单中显式列出的文件，不执行 archive.extractall。
    with tempfile.TemporaryDirectory(prefix="ffmpeg-", dir=cache) as temporary:
        stage = Path(temporary)
        for relative, asset in spec["files"].items():
            source = download(asset, cache)
            output = stage / relative
            output.parent.mkdir(parents=True, exist_ok=True)
            if "member" in asset:
                with zipfile.ZipFile(source) as archive, archive.open(asset["member"]) as stream, output.open("wb") as out:
                    shutil.copyfileobj(stream, out)
            else:
                shutil.copyfile(source, output)
        if "readme" in spec:
            (stage / "FFmpeg-README.txt").write_text(spec["readme"], encoding="utf-8")
        # LGPLv3 的补充条款引用 GPLv3，随包保留完整 GPLv3 文本。
        if spec.get("append_gplv3"):
            license_path = stage / "FFmpeg-LICENSE.txt"
            with license_path.open("ab") as out:
                out.write(b"\n\n")
                out.write((ROOT / "vendor/ffmpeg/GPL-3.0.txt").read_bytes())
        destination.mkdir(parents=True, exist_ok=True)
        for source in stage.rglob("*"):
            if source.is_file():
                output = destination / source.relative_to(stage)
                output.parent.mkdir(parents=True, exist_ok=True)
                os.replace(source, output)
        binary.chmod(binary.stat().st_mode | 0o755)
    print(f"FFmpeg 已就绪：{target}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True, help="Rust 目标三元组")
    parser.add_argument("--cache-dir", type=Path, default=ROOT / "target/ffmpeg-downloads")
    args = parser.parse_args()
    prepare(args.target, args.cache_dir)
