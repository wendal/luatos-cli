#!/usr/bin/env python3
"""从固定源码构建 HZV 精简 FFmpeg，默认只生成产物，不自动覆盖随包资源。"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import zipfile

from prepare_ffmpeg import ROOT, download


def shell_path(path):
    path = path.resolve().as_posix()
    return f"/{path[0].lower()}{path[2:]}" if os.name == "nt" and path[1:3] == ":/" else path


def write_notices(destination, ffmpeg_source, lame_source, versions):
    license_text = (ffmpeg_source / "COPYING.LGPLv2.1").read_bytes()
    license_text += b"\n\n===== LAME =====\n\n" + (lame_source / "COPYING").read_bytes()
    (destination / "FFmpeg-LICENSE.txt").write_bytes(license_text)
    # --enable-small 的程序可能不打印 buildconf，直接从构建生成的头文件保留完整参数。
    configuration = next(line.removeprefix("#define FFMPEG_CONFIGURATION ").strip()
                         for line in (destination / "build-config.h").read_text(encoding="utf-8").splitlines()
                         if line.startswith("#define FFMPEG_CONFIGURATION "))
    source_links = "\n".join(f"{key}: {spec['url']}\nSHA256: {spec['sha256']}" for key, spec in versions.items())
    readme = (
        "LuatOS CLI HZV minimal FFmpeg\n"
        f"FFmpeg {versions['ffmpeg']['version']} + static LAME {versions['lame']['version']}\n"
        "License: FFmpeg LGPL-2.1-or-later; LAME LGPL-2.0-or-later. See FFmpeg-LICENSE.txt.\n"
        "Rebuild: python scripts/build_hzv_ffmpeg.py\n"
        "Build options: scripts/build_hzv_ffmpeg.sh\n"
        "Input: local common media containers, H.264/HEVC/MPEG-4/VP8/VP9/WMV and common audio.\n"
        "Output: MJPEG, MP3 (libmp3lame), PCM S16LE only.\n"
        "No network, devices, hardware acceleration, subtitles, AV1, or general-purpose video encoding.\n\n"
        f"Sources:\n{source_links}\n\nConfiguration:\n{configuration}"
    )
    (destination / "FFmpeg-README.txt").write_text(readme, encoding="utf-8")


def build(work_dir, bash):
    versions = json.loads((ROOT / "scripts/ffmpeg-minimal-sources.json").read_text(encoding="utf-8"))
    work_dir = work_dir.resolve()
    if any(character.isspace() for character in str(work_dir)):
        raise RuntimeError("FFmpeg 构建目录请使用不含空格的路径（可用 --work-dir 指定）")
    cache = work_dir / "downloads"
    cache.mkdir(parents=True, exist_ok=True)
    sources = work_dir / "src"
    sources.mkdir(exist_ok=True)
    for name in ["ffmpeg", "lame"]:
        spec = versions[name]
        archive = download(spec, cache)
        if not (sources / spec["directory"] / "configure").is_file():
            with tarfile.open(archive) as tar:
                tar.extractall(sources, filter="data")
    env = os.environ.copy()
    if os.name == "nt":
        gcc = shutil.which("gcc")
        if bash is None and gcc:
            bash = Path(gcc).parents[2] / "usr/bin/bash.exe"
        if bash is None or not bash.is_file():
            raise RuntimeError("Windows 构建需要 MSYS2 UCRT64 gcc/make/pkg-config，可用 --bash 指定 MSYS2 bash")
        spec = versions["nasm_windows"]
        archive = download(spec, cache)
        tools = work_dir / "tools"
        if not (tools / spec["directory"] / "nasm.exe").is_file():
            with zipfile.ZipFile(archive) as zip_file:
                zip_file.extractall(tools)
        msys_root = bash.resolve().parents[2]
        paths = [msys_root / "ucrt64/bin", msys_root / "usr/bin", tools / spec["directory"]]
        env["PATH"] = os.pathsep.join(map(str, paths)) + os.pathsep + env.get("PATH", "")
    else:
        bash = bash or Path(shutil.which("bash") or "/bin/bash")
    destination = work_dir / "result"
    arguments = [bash, "--noprofile", "--norc", shell_path(ROOT / "scripts/build_hzv_ffmpeg.sh"),
                 shell_path(sources / versions["ffmpeg"]["directory"]), shell_path(sources / versions["lame"]["directory"]),
                 shell_path(work_dir / "build"), shell_path(destination)]
    subprocess.run(list(map(str, arguments)), check=True, env=env)
    write_notices(destination, sources / versions["ffmpeg"]["directory"], sources / versions["lame"]["directory"], versions)
    binary = destination / ("bin/ffmpeg.exe" if os.name == "nt" else "bin/ffmpeg")
    with binary.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    print(f"FFmpeg: {binary}\n大小: {binary.stat().st_size} bytes\nSHA256: {digest}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--work-dir", type=Path, default=ROOT / "target/ffmpeg-minimal")
    parser.add_argument("--bash", type=Path)
    args = parser.parse_args()
    build(args.work_dir, args.bash)
