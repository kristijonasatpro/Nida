#!/usr/bin/env python3
"""Install the checksum-verified FFmpeg 9 shared SDK used by Linux builds."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shlex
import shutil
import subprocess
import tarfile
import tempfile
import urllib.request

ASSET = "ffmpeg-n9.0-latest-linux64-gpl-shared-9.0.tar.xz"
RELEASE = "https://api.github.com/repos/BtbN/FFmpeg-Builds/releases/tags/latest"


def resolve():
    headers = {"User-Agent": "Nida-Linux-build"}
    token = os.environ.get("GITHUB_TOKEN") or os.environ.get("GH_TOKEN")
    if token:
        headers["Authorization"] = f"Bearer {token}"
    with urllib.request.urlopen(urllib.request.Request(RELEASE, headers=headers), timeout=60) as reply:
        assets = json.load(reply)["assets"]
    matches = [asset for asset in assets if asset["name"] == ASSET]
    if len(matches) != 1 or not re.fullmatch(r"sha256:[0-9a-f]{64}", matches[0].get("digest", "")):
        raise RuntimeError("FFmpeg 9 shared SDK or its published SHA256 is unavailable")
    asset = matches[0]
    return {"url": asset["browser_download_url"], "sha256": asset["digest"][7:],
            "published": asset["updated_at"], "recipes": "https://github.com/BtbN/FFmpeg-Builds"}


def environment(prefix):
    env = os.environ.copy()
    for name, directory in [("PATH", "bin"), ("PKG_CONFIG_PATH", "lib/pkgconfig"),
                            ("LD_LIBRARY_PATH", "lib")]:
        env[name] = str(prefix / directory) + (os.pathsep + env[name] if env.get(name) else "")
    env["FFMPEG_DIR"] = str(prefix)
    return env


def verify(prefix):
    env = environment(prefix)
    for tool in ["ffmpeg", "ffprobe"]:
        version = subprocess.check_output([str(prefix / "bin" / tool), "-version"], env=env, text=True)
        if not re.match(rf"{tool} version n?9\.", version):
            raise RuntimeError(f"{tool} is not FFmpeg 9")
        print(version.splitlines()[0])
    subprocess.run(["pkg-config", "--atleast-version=63", "libavcodec"], env=env, check=True)
    if not (prefix / "include/libavutil/frame.h").is_file():
        raise RuntimeError("FFmpeg development headers missing")
    # Compile and execute the API that failed with Ubuntu's FFmpeg 6.1.
    with tempfile.TemporaryDirectory(prefix="nida-ffmpeg-api-") as directory:
        source = Path(directory) / "check.c"
        source.write_text("""#include <libavutil/frame.h>
int main(void) {
    AVFrame *frame = av_frame_alloc();
    if (!frame) return 1;
    if (!av_frame_new_side_data(frame, AV_FRAME_DATA_CONTENT_LIGHT_LEVEL, 4)) return 2;
    av_frame_side_data_free(&frame->side_data, &frame->nb_side_data);
    int failed = frame->nb_side_data != 0;
    av_frame_free(&frame);
    return failed;
}
""")
        binary = Path(directory) / "check"
        subprocess.run([os.environ.get("CC", "cc"), "-Werror", f"-I{prefix / 'include'}",
                        str(source), f"-L{prefix / 'lib'}", "-lavutil", "-o", str(binary)], check=True)
        subprocess.run([str(binary)], env=env, check=True)


def install(prefix, metadata):
    marker = prefix / "nida-build-inputs.json"
    if prefix.exists():
        if not marker.is_file():
            raise RuntimeError(f"Refusing to replace a directory not created by this installer: {prefix}")
        if json.loads(marker.read_text()) == metadata:
            return
    prefix.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="nida-ffmpeg-", dir=prefix.parent) as directory:
        directory = Path(directory)
        archive = directory / ASSET
        urllib.request.urlretrieve(metadata["url"], archive)
        with archive.open("rb") as stream:
            if hashlib.file_digest(stream, "sha256").hexdigest() != metadata["sha256"]:
                raise RuntimeError("FFmpeg archive checksum mismatch")
        unpacked = directory / "unpacked"
        with tarfile.open(archive) as bundle:
            bundle.extractall(unpacked, filter="data")
        roots = list(unpacked.iterdir())
        if len(roots) != 1 or not (roots[0] / "include/libavcodec/avcodec.h").is_file():
            raise RuntimeError("Unexpected FFmpeg SDK layout")
        (roots[0] / marker.name).write_text(json.dumps(metadata, indent=2) + "\n")
        if prefix.exists():
            prefix.rename(directory / "previous")
        shutil.move(roots[0], prefix)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--prefix", type=Path, default=Path.home() / ".local/nida-ffmpeg-9")
    parser.add_argument("--metadata", type=Path)
    parser.add_argument("--resolve-only", action="store_true")
    args = parser.parse_args()
    if platform.system() != "Linux" or platform.machine() not in ("x86_64", "AMD64"):
        parser.error("This SDK is for Linux x86_64; other architectures need a native FFmpeg 9 build")
    metadata = json.loads(args.metadata.read_text()) if args.metadata and args.metadata.exists() else resolve()
    if args.metadata:
        args.metadata.write_text(json.dumps(metadata, indent=2) + "\n")
    if args.resolve_only:
        if os.environ.get("GITHUB_OUTPUT"):
            with open(os.environ["GITHUB_OUTPUT"], "a") as output:
                output.write(f"sha256={metadata['sha256']}\n")
        return
    prefix = args.prefix.expanduser().resolve()
    install(prefix, metadata)
    verify(prefix)
    # Source this file for local builds; Actions uses its environment files.
    exports = []
    for name, directory in [("PATH", "bin"), ("PKG_CONFIG_PATH", "lib/pkgconfig"), ("LD_LIBRARY_PATH", "lib")]:
        exports.append(f'export {name}={shlex.quote(str(prefix / directory))}${{{name}:+:${name}}}')
    exports.append(f"export FFMPEG_DIR={shlex.quote(str(prefix))}")
    (prefix / "nida-env.sh").write_text("\n".join(exports) + "\n")
    if os.environ.get("GITHUB_ENV"):
        env = environment(prefix)
        with open(os.environ["GITHUB_ENV"], "a") as output:
            for name in ["PKG_CONFIG_PATH", "LD_LIBRARY_PATH", "FFMPEG_DIR"]:
                output.write(f"{name}={env[name]}\n")
        with open(os.environ["GITHUB_PATH"], "a") as output:
            output.write(str(prefix / "bin") + "\n")
    print(f"FFmpeg SDK ready. For this shell: source {shlex.quote(str(prefix / 'nida-env.sh'))}")


if __name__ == "__main__":
    main()
