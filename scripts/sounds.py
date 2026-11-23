#!/usr/bin/env python3
"""Makes the notification tones in android/app/src/main/res/raw (desktop includes them from there).

Short and soft, in the manner of a phone's message tones: mallet, glass and bubble sounds with a
2 to 4 ms attack, a small room around them, and a fade to true silence, so nothing clicks. Every
tone comes out equally loud (the same RMS over its first 300 ms, -16 dBFS, the peak under -1 dBFS), so any of
them suits any kind of chat. Deterministic: the room's noise has a fixed seed.
Needs numpy and ffmpeg with libvorbis. Usage: scripts/sounds.py
"""
import os
import subprocess
import tempfile
import wave

import numpy as np

SR = 48000
OUT = os.path.join(os.path.dirname(__file__), "..", "android", "app", "src", "main", "res", "raw")
RNG = np.random.default_rng(7)


def note(f):
    """Frequency of a MIDI note."""
    return 440.0 * 2 ** ((f - 69) / 12)


def env(t, attack, tau):
    return np.minimum(1, t / attack) * np.exp(-t / tau)


def lowpass(x, cutoff):
    """One pole: takes the edge off without colouring the tone."""
    a = np.exp(-2 * np.pi * cutoff / SR)
    y = np.empty_like(x)
    acc = 0.0
    for i, v in enumerate(x):
        acc = (1 - a) * v + a * acc
        y[i] = acc
    return y


def marimba(f, dur=0.5):
    t = np.arange(int(SR * dur)) / SR
    tau = 0.16 * (880 / f) ** 0.4
    s = (np.sin(2 * np.pi * f * t) * env(t, 0.002, tau)
         + 0.16 * np.sin(2 * np.pi * 3.93 * f * t) * env(t, 0.001, tau / 4)
         + 0.04 * np.sin(2 * np.pi * 9.2 * f * t) * env(t, 0.001, tau / 12))
    mallet = lowpass(RNG.standard_normal(len(t)), 3000) * env(t, 0.0005, 0.004) * 0.05
    return s + mallet


def glass(f, dur=1.0):
    t = np.arange(int(SR * dur)) / SR
    return (np.sin(2 * np.pi * f * t) * env(t, 0.003, 0.32)
            + 0.22 * np.sin(2 * np.pi * 2.0 * f * t) * env(t, 0.003, 0.16)
            + 0.10 * np.sin(2 * np.pi * 2.76 * f * t) * env(t, 0.002, 0.08)
            + 0.03 * np.sin(2 * np.pi * 5.4 * f * t) * env(t, 0.001, 0.03))


def wood(f, dur=0.25):
    """A hollow knock: a low fundamental with an inharmonic overtone, gone in a moment."""
    t = np.arange(int(SR * dur)) / SR
    return (np.sin(2 * np.pi * f * t) * env(t, 0.001, 0.045)
            + 0.35 * np.sin(2 * np.pi * 2.57 * f * t) * env(t, 0.001, 0.02)
            + lowpass(RNG.standard_normal(len(t)), 1800) * env(t, 0.0003, 0.003) * 0.08)


def pluck(f, dur=0.45):
    t = np.arange(int(SR * dur)) / SR
    return (np.sin(2 * np.pi * f * t) * env(t, 0.002, 0.12)
            + 0.3 * np.sin(2 * np.pi * 2 * f * t) * env(t, 0.002, 0.05)
            + 0.08 * np.sin(2 * np.pi * 3 * f * t) * env(t, 0.002, 0.025))


def pad(freqs, dur=0.9):
    t = np.arange(int(SR * dur)) / SR
    swell = np.minimum(1, t / 0.05) * np.exp(-t / 0.28)
    s = sum(np.sin(2 * np.pi * f * t) + 0.5 * np.sin(2 * np.pi * f * 1.003 * t) for f in freqs)
    return s * swell


def bubble(f0, f1, dur=0.18):
    """A pitch that rises fast, as a bubble's does when it pops."""
    t = np.arange(int(SR * dur)) / SR
    f = f1 + (f0 - f1) * np.exp(-t / 0.018)
    phase = 2 * np.pi * np.cumsum(f) / SR
    return np.sin(phase) * env(t, 0.001, 0.03)


def seq(parts, total):
    out = np.zeros(int(SR * total))
    for start, sound, gain in parts:
        i = int(SR * start)
        n = min(len(sound), len(out) - i)
        out[i:i + n] += gain * sound[:n]
    return out


def room(x, mix=0.16):
    """A small soft room: the sound convolved with fading, darkened noise."""
    n = int(SR * 0.45)
    t = np.arange(n) / SR
    ir = lowpass(RNG.standard_normal(n), 4000) * np.exp(-t / 0.09)
    ir /= np.sqrt(np.sum(ir ** 2))
    wet = np.fft.irfft(np.fft.rfft(x, len(x) + n) * np.fft.rfft(ir, len(x) + n))[: len(x) + n]
    dry = np.concatenate([x, np.zeros(n)])
    return dry + mix * wet


def finish(x):
    x = lowpass(room(x), 9000)
    # Ends where it is quieter than -70 dB under its peak, then fades the last 40 ms to zero.
    loud = np.nonzero(np.abs(x) > np.max(np.abs(x)) * 10 ** (-70 / 20))[0]
    x = x[: loud[-1] + 1]
    fade = min(len(x), int(SR * 0.04))
    x[-fade:] *= np.linspace(1, 0, fade) ** 2
    head = x[: int(SR * 0.3)]
    x = x * 10 ** (-16 / 20) / np.sqrt(np.mean(head ** 2))
    peak = np.max(np.abs(x))
    if peak > 10 ** (-1 / 20):
        x *= 10 ** (-1 / 20) / peak
    return x


def save(name, x):
    x = finish(x)
    os.makedirs(OUT, exist_ok=True)
    with tempfile.TemporaryDirectory() as tmp:
        wav = os.path.join(tmp, "a.wav")
        with wave.open(wav, "wb") as w:
            w.setnchannels(1)
            w.setsampwidth(2)
            w.setframerate(SR)
            w.writeframes((x * 32767).astype("<i2").tobytes())
        out = os.path.join(OUT, f"tone_{name}.ogg")
        subprocess.run(["ffmpeg", "-nostdin", "-loglevel", "error", "-y", "-i", wav, "-c:a", "libvorbis", "-q:a", "4", "-map_metadata", "-1", out], check=True)
    print(f"{name}: {len(x) / SR:.2f} s")


# Dew: three quick mallet notes rising, the direct-message default.
save("dew", seq([(0, marimba(note(84)), 0.8), (0.07, marimba(note(88)), 0.75), (0.14, marimba(note(91)), 0.9)], 0.75))
# Pebble: two soft knocks, the second higher, the group default.
save("pebble", seq([(0, wood(note(67)), 1), (0.085, wood(note(74)), 0.8)], 0.4))
# Chime: two glass notes a fifth apart, which ring a little longer; the mention default.
save("chime", seq([(0, glass(note(86)), 0.9), (0.11, glass(note(93)), 0.8)], 1.1))
# Ripple: four plucked notes falling through a pentatonic scale.
save("ripple", seq([(i * 0.055, pluck(note(m)), 1 - 0.12 * i) for i, m in enumerate([91, 88, 86, 84])], 0.7))
# Bloom: a soft major ninth chord that swells and fades.
save("bloom", pad([note(m) for m in (72, 76, 79, 86)], 1.0))
# Pop: one bubble, then a smaller one.
save("pop", seq([(0, bubble(note(69), note(81)), 1), (0.09, bubble(note(76), note(88)), 0.6)], 0.35))
