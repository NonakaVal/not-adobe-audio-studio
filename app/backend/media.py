from __future__ import annotations

import json
import mimetypes
import subprocess
from array import array
from pathlib import Path
from typing import Any


def probe(path: Path) -> dict[str, Any]:
    result = subprocess.run(
        [
            "ffprobe", "-v", "error", "-show_format", "-show_streams",
            "-of", "json", str(path),
        ],
        capture_output=True,
        text=True,
        check=True,
        timeout=30,
    )
    info = json.loads(result.stdout)
    streams = info.get("streams", [])
    video = next((stream for stream in streams if stream.get("codec_type") == "video"), None)
    audio = next((stream for stream in streams if stream.get("codec_type") == "audio"), None)
    fps = 0.0
    if video:
        rate = video.get("avg_frame_rate", "0/1").split("/")
        if len(rate) == 2 and float(rate[1] or 0):
            fps = float(rate[0]) / float(rate[1])
    return {
        "duration": float(info.get("format", {}).get("duration", 0)),
        "has_video": video is not None,
        "has_audio": audio is not None,
        "width": int(video.get("width", 0)) if video else 0,
        "height": int(video.get("height", 0)) if video else 0,
        "fps": fps,
        "audio_codec": audio.get("codec_name", "") if audio else "",
        "mime_type": mimetypes.guess_type(path.name)[0] or "application/octet-stream",
    }


def waveform(path: Path, rate: int, duration: float) -> list[dict[str, float]]:
    sample_rate = 8000
    bucket_count = max(1, int(duration * rate))
    minimums = [1.0] * bucket_count
    maximums = [-1.0] * bucket_count
    command = [
        "ffmpeg", "-v", "error", "-i", str(path), "-vn", "-ac", "1",
        "-ar", str(sample_rate), "-f", "s16le", "pipe:1",
    ]
    process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    assert process.stdout is not None
    sample_index = 0
    remainder = b""
    while True:
        chunk = process.stdout.read(65536)
        if not chunk:
            break
        chunk = remainder + chunk
        even_length = len(chunk) - (len(chunk) % 2)
        samples = array("h")
        samples.frombytes(chunk[:even_length])
        remainder = chunk[even_length:]
        for sample in samples:
            bucket = min(bucket_count - 1, int(sample_index * rate / sample_rate))
            value = sample / 32768
            minimums[bucket] = min(minimums[bucket], value)
            maximums[bucket] = max(maximums[bucket], value)
            sample_index += 1
    stderr = process.stderr.read() if process.stderr else b""
    code = process.wait(timeout=60)
    if code:
        raise RuntimeError(stderr.decode("utf-8", errors="replace")[-2000:])
    return [
        {"min": minimum, "max": maximum}
        for minimum, maximum in zip(minimums, maximums)
        if minimum <= maximum
    ]
