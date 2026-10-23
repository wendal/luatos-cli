#!/usr/bin/env python3
"""在源码目录之外、空 PATH 环境验证随包 HZV 转换，不使用系统 FFmpeg。"""

import argparse
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import tempfile


def check(binary):
    binary = binary.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="luatos-hzv-bundle-") as temporary:
        root = Path(temporary)
        executable = root / binary.name
        shutil.copy2(binary, executable)
        shutil.copytree(binary.parent / "tools", root / "tools")
        arch = {"arm64": "aarch64", "AMD64": "x86_64"}.get(platform.machine(), platform.machine())
        name = "ffmpeg.exe" if os.name == "nt" else "ffmpeg"
        ffmpeg = root / "tools/ffmpeg" / arch / "bin" / name
        if not ffmpeg.is_file():
            ffmpeg = root / "tools/ffmpeg/bin" / name
        # 不继承 FFmpeg 显式覆盖，不能靠 PATH 中的程序掩盖缺失资源。
        env = {key: value for key, value in os.environ.items() if key.upper() not in {"PATH", "LUATOS_FFMPEG"}}
        env["PATH"] = ""
        # 精简版不包含测试图生成、AAC/MPEG-4 输出编码器，测试使用预生成的合成媒体。
        assert ffmpeg.is_file(), f"随包 FFmpeg 缺失：{ffmpeg}"
        testdata = Path(__file__).resolve().parent / "testdata"
        fixtures = [testdata / "hzv-source.mp4", *sorted((testdata / "media").glob("*"))]
        for fixture in fixtures:
            source = root / fixture.name
            output = root / f"{fixture.name}.hzv"
            shutil.copyfile(fixture, source)
            for action in [["probe", source], ["convert", source, "-o", output], ["verify", output, "--deep"]]:
                result = subprocess.run([str(executable), "--format", "json", "hzv", *map(str, action)],
                                        cwd=root, env=env, capture_output=True, encoding="utf-8")
                if result.returncode:
                    raise RuntimeError(f"HZV 验证失败：{fixture.name} / {action[0]}\n{result.stdout}\n{result.stderr}")
                assert json.loads(result.stdout)["status"] == "ok", result.stdout
        profiles = [
            ("none", ["--audio-codec", "none", "--no-packet-crc", "--loop-hint", "--fps", "30000/1001", "--scale-mode", "stretch"], 0),
            ("pcm", ["--audio-codec", "pcm_s16le", "--audio-rate", "16000", "--audio-channels", "2", "--scale-mode", "cover"], 2),
            ("pad", ["--audio-codec", "pcm_s16le", "--end-policy", "pad-audio", "--start-seconds", "0.1", "--duration-seconds", "0.4"], 2),
            ("short", ["--end-policy", "shortest", "--fps", "24", "--video-quality", "3"], 1),
        ]
        source = root / "hzv-source.mp4"
        for name, options, codec in profiles:
            output = root / f"profile-{name}.hzv"
            for action in [["convert", source, "-o", output, *options], ["verify", output, "--deep"]]:
                result = subprocess.run([str(executable), "--format", "json", "hzv", *map(str, action)],
                                        cwd=root, env=env, capture_output=True, encoding="utf-8")
                if result.returncode:
                    raise RuntimeError(f"HZV 参数验证失败：{name}\n{result.stdout}\n{result.stderr}")
                data = json.loads(result.stdout)
                assert data["status"] == "ok" and data["data"]["header"]["audio_codec"] == codec, result.stdout
        print(f"随包 HZV 验证通过：{len(fixtures)} 个输入、{len(profiles)} 组参数；程序已迁移到临时目录，PATH 为空，无显式 FFmpeg 路径")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin", type=Path, required=True)
    check(parser.parse_args().bin)
