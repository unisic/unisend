#!/usr/bin/env python3
"""Makes the notification sounds in android/app/src/main/res/raw (desktop includes them from there).

Soft glass bells: a direct message rises a major third, a mention a major triad, a group message is
one quieter note. Each note has a 4 ms attack and fades to silence, so nothing clicks at its end.
Needs numpy and ffmpeg with libvorbis. Usage: scripts/sounds.py
"""
import os
import subprocess
import tempfile
import wave

import numpy as np

SR = 48000
OUT = os.path.join(os.path.dirname(__file__), "..", "android", "app", "src", "main", "res", "raw")


def bell(f, dur, amp):
    t = np.arange(int(SR * dur)) / SR
    # The fundamental, a soft octave and an inharmonic partial, each decaying faster than the last.
    s = (np.sin(2 * np.pi * f * t) * np.exp(-t / 0.22)
         + 0.28 * np.sin(2 * np.pi * 2 * f * t) * np.exp(-t / 0.10)
         + 0.08 * np.sin(2 * np.pi * 2.76 * f * t) * np.exp(-t / 0.05))
    attack = np.minimum(1, t / 0.004)
    release = np.clip((dur - t) / 0.35, 0, 1) ** 2
    return amp * s * attack * release


def mix(notes, total):
    out = np.zeros(int(SR * total))
    for start, f, amp, dur in notes:
        b = bell(f, dur, amp)
        i = int(SR * start)
        n = min(len(b), len(out) - i)
        out[i:i + n] += b[:n]
    return out


def save(name, x, peak_db):
    x = x / np.max(np.abs(x)) * 10 ** (peak_db / 20)
    with tempfile.TemporaryDirectory() as tmp:
        wav = os.path.join(tmp, "a.wav")
        with wave.open(wav, "wb") as w:
            w.setnchannels(1)
            w.setsampwidth(2)
            w.setframerate(SR)
            w.writeframes((x * 32767).astype("<i2").tobytes())
        out = os.path.join(OUT, f"notify_{name}.ogg")
        subprocess.run(["ffmpeg", "-nostdin", "-loglevel", "error", "-y", "-i", wav, "-c:a", "libvorbis", "-q:a", "5", "-map_metadata", "-1", out], check=True)


C6, E6, G6, A5 = 1046.50, 1318.51, 1567.98, 880.0
save("direct", mix([(0, C6, 1, 0.9), (0.09, E6, 0.9, 0.9)], 1.0), -4)
save("group", mix([(0, A5, 1, 0.8)], 0.8), -9)
save("mention", mix([(0, C6, 1, 0.9), (0.08, E6, 0.9, 0.9), (0.16, G6, 0.85, 0.9)], 1.07), -3)
