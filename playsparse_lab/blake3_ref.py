"""Small dependency-free BLAKE3 reference implementation for PlaySparse experiments.

This is deliberately a readable research implementation, not production cryptographic
code. Production PlaySparse should use the official BLAKE3 Rust crate.
"""
from __future__ import annotations

from dataclasses import dataclass

IV = [
    0x6A09E667, 0xBB67AE85, 0x3C6EF372, 0xA54FF53A,
    0x510E527F, 0x9B05688C, 0x1F83D9AB, 0x5BE0CD19,
]
MSG_PERMUTATION = [2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8]
CHUNK_START = 1 << 0
CHUNK_END = 1 << 1
PARENT = 1 << 2
ROOT = 1 << 3
BLOCK_LEN = 64
CHUNK_LEN = 1024
MASK32 = 0xFFFFFFFF


def _rotr32(x: int, n: int) -> int:
    return ((x >> n) | ((x << (32 - n)) & MASK32)) & MASK32


def _g(state: list[int], a: int, b: int, c: int, d: int, mx: int, my: int) -> None:
    state[a] = (state[a] + state[b] + mx) & MASK32
    state[d] = _rotr32(state[d] ^ state[a], 16)
    state[c] = (state[c] + state[d]) & MASK32
    state[b] = _rotr32(state[b] ^ state[c], 12)
    state[a] = (state[a] + state[b] + my) & MASK32
    state[d] = _rotr32(state[d] ^ state[a], 8)
    state[c] = (state[c] + state[d]) & MASK32
    state[b] = _rotr32(state[b] ^ state[c], 7)


def _round(state: list[int], m: list[int]) -> None:
    _g(state, 0, 4, 8, 12, m[0], m[1])
    _g(state, 1, 5, 9, 13, m[2], m[3])
    _g(state, 2, 6, 10, 14, m[4], m[5])
    _g(state, 3, 7, 11, 15, m[6], m[7])
    _g(state, 0, 5, 10, 15, m[8], m[9])
    _g(state, 1, 6, 11, 12, m[10], m[11])
    _g(state, 2, 7, 8, 13, m[12], m[13])
    _g(state, 3, 4, 9, 14, m[14], m[15])


def _permute(m: list[int]) -> list[int]:
    return [m[i] for i in MSG_PERMUTATION]


def _words_from_block(block: bytes) -> list[int]:
    padded = block + b"\x00" * (BLOCK_LEN - len(block))
    return [int.from_bytes(padded[i:i+4], "little") for i in range(0, BLOCK_LEN, 4)]


def _compress(cv: list[int], block_words: list[int], counter: int, block_len: int, flags: int) -> list[int]:
    state = cv[:] + IV[:4] + [counter & MASK32, (counter >> 32) & MASK32, block_len, flags]
    m = block_words[:]
    for round_index in range(7):
        _round(state, m)
        if round_index != 6:
            m = _permute(m)
    for i in range(8):
        state[i] ^= state[i + 8]
        state[i + 8] ^= cv[i]
    return [x & MASK32 for x in state]


@dataclass
class _Output:
    input_cv: list[int]
    block_words: list[int]
    counter: int
    block_len: int
    flags: int

    def chaining_value(self) -> list[int]:
        return _compress(self.input_cv, self.block_words, self.counter, self.block_len, self.flags)[:8]

    def root_bytes(self, length: int = 32) -> bytes:
        out = bytearray()
        output_block_counter = 0
        while len(out) < length:
            words = _compress(
                self.input_cv,
                self.block_words,
                output_block_counter,
                self.block_len,
                self.flags | ROOT,
            )
            block = b"".join(w.to_bytes(4, "little") for w in words)
            out.extend(block)
            output_block_counter += 1
        return bytes(out[:length])


def _chunk_output(chunk: bytes, chunk_counter: int) -> _Output:
    cv = IV[:]
    if not chunk:
        return _Output(cv, _words_from_block(b""), chunk_counter, 0, CHUNK_START | CHUNK_END)
    blocks = [chunk[i:i+BLOCK_LEN] for i in range(0, len(chunk), BLOCK_LEN)]
    for i, block in enumerate(blocks[:-1]):
        flags = CHUNK_START if i == 0 else 0
        cv = _compress(cv, _words_from_block(block), chunk_counter, BLOCK_LEN, flags)[:8]
    last = blocks[-1]
    flags = CHUNK_END
    if len(blocks) == 1:
        flags |= CHUNK_START
    return _Output(cv, _words_from_block(last), chunk_counter, len(last), flags)


def _parent_output(left_cv: list[int], right_cv: list[int]) -> _Output:
    return _Output(IV[:], left_cv + right_cv, 0, BLOCK_LEN, PARENT)


def _add_chunk_cv(stack: list[list[int]], new_cv: list[int], total_chunks: int) -> None:
    while total_chunks & 1 == 0:
        left = stack.pop()
        new_cv = _parent_output(left, new_cv).chaining_value()
        total_chunks >>= 1
    stack.append(new_cv)


def digest(data: bytes, length: int = 32) -> bytes:
    chunks = [data[i:i+CHUNK_LEN] for i in range(0, len(data), CHUNK_LEN)]
    if not chunks:
        chunks = [b""]
    stack: list[list[int]] = []
    for i, chunk in enumerate(chunks[:-1]):
        cv = _chunk_output(chunk, i).chaining_value()
        _add_chunk_cv(stack, cv, i + 1)
    output = _chunk_output(chunks[-1], len(chunks) - 1)
    while stack:
        output = _parent_output(stack.pop(), output.chaining_value())
    return output.root_bytes(length)


def hexdigest(data: bytes, length: int = 32) -> str:
    return digest(data, length).hex()
