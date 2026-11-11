#!/usr/bin/env python3
"""Makes a small test album (WAV files with title/artist/album tags) for the
Android emulator run: three 24-second songs of soft chords."""
import math
import struct
import sys
from pathlib import Path

RATE = 44100
SECONDS = 24


def info_chunk(tags):
    data = b"INFO"
    for key, value in tags.items():
        text = value.encode("utf-8") + b"\0"
        if len(text) % 2:
            text += b"\0"
        data += key.encode("ascii") + struct.pack("<I", len(text)) + text
    return b"LIST" + struct.pack("<I", len(data)) + data


def song(path, base, title, number):
    frames = bytearray()
    for n in range(RATE * SECONDS):
        t = n / RATE
        fade = min(1.0, t / 0.5, (SECONDS - t) / 0.5)
        v = sum(math.sin(2 * math.pi * base * k * t) / (k * 1.5) for k in (1, 1.5, 2, 3))
        sample = int(max(-1.0, min(1.0, 0.22 * fade * v)) * 32767)
        frames += struct.pack("<hh", sample, sample)
    fmt = struct.pack("<HHIIHH", 1, 2, RATE, RATE * 4, 4, 16)
    tags = info_chunk({"INAM": title, "IART": "The Test Tones", "IPRD": "Emulator Sessions", "ITRK": str(number), "ICRD": "2026"})
    body = b"WAVE" + b"fmt " + struct.pack("<I", len(fmt)) + fmt + tags + b"data" + struct.pack("<I", len(frames)) + bytes(frames)
    path.write_bytes(b"RIFF" + struct.pack("<I", len(body)) + body)


out = Path(sys.argv[1] if len(sys.argv) > 1 else "test-songs")
out.mkdir(parents=True, exist_ok=True)
for i, (base, title) in enumerate([(220.0, "First Light"), (277.2, "Second Wind"), (329.6, "Third Coast")], start=1):
    song(out / f"{i}.wav", base, title, i)
    print("made", out / f"{i}.wav")
