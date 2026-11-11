#!/usr/bin/env python3
"""Builds the startup animation assets from the rendered HyperFrames video.

    python3 tools/make_intro.py [crates/unitn/assets/intro/unitn-sting.mp4]

Writes, next to the video:
  frames.bin  the first INTRO_SECONDS as JPEG frames (u32 count, u32 sizes, data; little endian)
  sound.m4a   the video's sound track (AAC, copied as is)
Needs ffmpeg.
"""
import struct, subprocess, sys, tempfile
from pathlib import Path

INTRO_SECONDS = 4.6  # the rest of the video is the still logo; the app holds the last frame
src = Path(sys.argv[1] if len(sys.argv) > 1 else "crates/unitn/assets/intro/unitn-sting.mp4")
out = src.parent
with tempfile.TemporaryDirectory() as tmp:
    subprocess.run(["ffmpeg", "-v", "error", "-i", str(src), "-t", str(INTRO_SECONDS),
                    "-r", "30", "-q:v", "4", f"{tmp}/%04d.jpg"], check=True)
    frames = [p.read_bytes() for p in sorted(Path(tmp).glob("*.jpg"))]
blob = struct.pack("<I", len(frames)) + b"".join(struct.pack("<I", len(f)) for f in frames) + b"".join(frames)
(out / "frames.bin").write_bytes(blob)
subprocess.run(["ffmpeg", "-v", "error", "-y", "-i", str(src), "-vn", "-c:a", "copy", str(out / "sound.m4a")], check=True)
print(f"{len(frames)} frames, {len(blob) / 1048576:.1f} MB; sound {(out / 'sound.m4a').stat().st_size // 1024} kB")
