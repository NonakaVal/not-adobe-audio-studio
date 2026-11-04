from __future__ import annotations

from dataclasses import dataclass
import math
from typing import Iterable


Interval = tuple[float, float]


def normalize(intervals: Iterable[Interval], duration: float) -> list[Interval]:
    result: list[Interval] = []
    for start, end in sorted(intervals):
        start = max(0.0, min(float(start), duration))
        end = max(0.0, min(float(end), duration))
        if end <= start:
            continue
        if result and start <= result[-1][1] + 1e-9:
            result[-1] = (result[-1][0], max(result[-1][1], end))
        else:
            result.append((start, end))
    return result


def normalize_ordered(intervals: Iterable[Interval], duration: float) -> list[Interval]:
    """Clamp EDL intervals without changing their playback order."""
    result: list[Interval] = []
    for start, end in intervals:
        start = max(0.0, min(float(start), duration))
        end = max(0.0, min(float(end), duration))
        if end <= start:
            continue
        if result and abs(start - result[-1][1]) <= 1e-9:
            result[-1] = (result[-1][0], end)
        else:
            result.append((start, end))
    return result


@dataclass
class EditDecisionList:
    duration: float
    kept: list[Interval]

    @classmethod
    def full(cls, duration: float) -> EditDecisionList:
        return cls(duration=max(0.0, duration), kept=[(0.0, max(0.0, duration))])

    def delete(self, start: float, end: float, timebase: float | None = None) -> None:
        if timebase and timebase > 0:
            start = math.floor(start * timebase + 1e-9) / timebase
            end = math.ceil(end * timebase - 1e-9) / timebase
        start = max(0.0, min(start, self.duration))
        end = max(0.0, min(end, self.duration))
        if end <= start:
            return
        kept: list[Interval] = []
        for left, right in self.kept:
            if right <= start or left >= end:
                kept.append((left, right))
                continue
            if left < start:
                kept.append((left, start))
            if right > end:
                kept.append((end, right))
        self.kept = normalize_ordered(kept, self.duration)

    def insert(
        self,
        intervals: list[Interval],
        target: float,
        timebase: float | None = None,
    ) -> None:
        target = max(0.0, min(float(target), self.duration))
        if not intervals:
            return
        if timebase and timebase > 0:
            target = min(math.floor(target * timebase + 0.5) / timebase, self.duration)
            intervals = [
                (
                    math.floor(start * timebase + 1e-9) / timebase,
                    math.ceil(end * timebase - 1e-9) / timebase,
                )
                for start, end in intervals
            ]
        if not self.kept:
            self.kept = normalize_ordered(intervals, self.duration)
            return
        for index, (start, end) in enumerate(self.kept):
            if start < target < end:
                self.kept[index:index + 1] = [(start, target), *intervals, (target, end)]
                self.kept = normalize_ordered(self.kept, self.duration)
                return
            if abs(target - start) <= 1e-9:
                self.kept[index:index] = intervals
                self.kept = normalize_ordered(self.kept, self.duration)
                return
        for index in range(len(self.kept) - 1, -1, -1):
            if abs(target - self.kept[index][1]) <= 1e-9:
                self.kept[index + 1:index + 1] = intervals
                self.kept = normalize_ordered(self.kept, self.duration)
                return

        # A playhead can land in a gap left by a previous cut. Insert at the
        # closest surviving boundary so paste remains usable after removals.
        boundaries = [boundary for part in self.kept for boundary in part]
        nearest = min(boundaries, key=lambda boundary: abs(boundary - target))
        for index, (start, end) in enumerate(self.kept):
            if abs(nearest - start) <= 1e-9:
                self.kept[index:index] = intervals
                self.kept = normalize_ordered(self.kept, self.duration)
                return
            if abs(nearest - end) <= 1e-9:
                self.kept[index + 1:index + 1] = intervals
                self.kept = normalize_ordered(self.kept, self.duration)
                return

    def edited_duration(self) -> float:
        return sum(end - start for start, end in self.kept)

    def is_deleted(self, timestamp: float) -> bool:
        return not any(start <= timestamp < end for start, end in self.kept)

    def to_dict(self) -> dict[str, object]:
        return {
            "duration": self.duration,
            "kept": [{"start": start, "end": end} for start, end in self.kept],
            "edited_duration": self.edited_duration(),
        }

    @classmethod
    def from_dict(cls, data: dict[str, object]) -> EditDecisionList:
        duration = max(0.0, float(data["duration"]))
        raw = data.get("kept", [])
        kept = normalize_ordered(
            (
                (float(item["start"]), float(item["end"]))
                for item in raw
                if isinstance(item, dict)
            ),
            duration,
        )
        return cls(duration=duration, kept=kept)


def original_to_edited(timestamp: float, kept: list[Interval]) -> float | None:
    offset = 0.0
    for start, end in kept:
        if start <= timestamp <= end:
            return offset + timestamp - start
        if timestamp < start:
            return None
        offset += end - start
    return None


def edited_to_original(timestamp: float, kept: list[Interval]) -> float:
    offset = 0.0
    for start, end in kept:
        segment_duration = end - start
        if timestamp <= offset + segment_duration:
            return start + max(0.0, timestamp - offset)
        offset += segment_duration
    return kept[-1][1] if kept else 0.0
