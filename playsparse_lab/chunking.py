"""Reference chunkers used by PlaySparse M0/M1 experiments."""
from __future__ import annotations

from dataclasses import dataclass
import math
from typing import Iterator

MASK64 = (1 << 64) - 1


def _splitmix64(x: int) -> int:
    x = (x + 0x9E3779B97F4A7C15) & MASK64
    z = x
    z = ((z ^ (z >> 30)) * 0xBF58476D1CE4E5B9) & MASK64
    z = ((z ^ (z >> 27)) * 0x94D049BB133111EB) & MASK64
    return (z ^ (z >> 31)) & MASK64


GEAR = [_splitmix64(0xC0FFEE + i) for i in range(256)]


@dataclass(frozen=True)
class Chunk:
    offset: int
    data: bytes

    @property
    def size(self) -> int:
        return len(self.data)


def fixed_chunks(data: bytes, size: int) -> Iterator[Chunk]:
    if size <= 0:
        raise ValueError("size must be positive")
    for offset in range(0, len(data), size):
        yield Chunk(offset, data[offset:offset + size])


def fastcdc_chunks(
    data: bytes,
    *,
    min_size: int = 64 * 1024,
    avg_size: int = 256 * 1024,
    max_size: int = 1024 * 1024,
) -> Iterator[Chunk]:
    """FastCDC-style normalized Gear chunking.

    This follows the FastCDC design principles (Gear rolling hash, skip until
    minimum size, normalized masks around the target average). It is a small
    research implementation, not claimed byte-identical with any specific
    FastCDC library. Experiment 02 measures CDC behavior, while production code
    will use a vetted Rust implementation.
    """
    if not (0 < min_size <= avg_size <= max_size):
        raise ValueError("require 0 < min_size <= avg_size <= max_size")
    bits = max(1, round(math.log2(avg_size)))
    strict_mask = (1 << min(63, bits + 1)) - 1
    loose_mask = (1 << max(1, bits - 1)) - 1
    n = len(data)
    start = 0
    while start < n:
        remaining = n - start
        if remaining <= min_size:
            yield Chunk(start, data[start:])
            break
        end = min(start + max_size, n)
        normal = min(start + avg_size, end)
        i = min(start + min_size, end)
        h = 0
        cut = end
        while i < normal:
            h = ((h << 1) + GEAR[data[i]]) & MASK64
            if (h & strict_mask) == 0:
                cut = i + 1
                break
            i += 1
        if cut == end:
            while i < end:
                h = ((h << 1) + GEAR[data[i]]) & MASK64
                if (h & loose_mask) == 0:
                    cut = i + 1
                    break
                i += 1
        yield Chunk(start, data[start:cut])
        start = cut
