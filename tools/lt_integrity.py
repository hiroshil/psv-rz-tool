#!/usr/bin/env python3
"""Verify or regenerate the PCSG00933 lt.bin integrity footer."""
from __future__ import annotations

import argparse
from pathlib import Path

FILE_SIZE = 0x0FD800
FOOTER_SIZE = 0x10
SEED = 0x1111111111111111
MASK = (1 << 64) - 1


def compute_footer(payload: bytes) -> bytes:
    if len(payload) % 0x10:
        raise ValueError(
            f"checksum payload has {len(payload):#x} bytes; expected a multiple of 0x10"
        )
    lane0 = SEED
    lane1 = SEED
    for offset in range(0, len(payload), 0x10):
        lane0 = (lane0 + int.from_bytes(payload[offset : offset + 8], "little")) & MASK
        lane1 = (lane1 + int.from_bytes(payload[offset + 8 : offset + 0x10], "little")) & MASK
    return lane0.to_bytes(8, "little") + lane1.to_bytes(8, "little")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("input", type=Path)
    parser.add_argument("output", type=Path, nargs="?")
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()

    data = bytearray(args.input.read_bytes())
    if len(data) != FILE_SIZE:
        raise SystemExit(f"{args.input}: size {len(data):#x}, expected {FILE_SIZE:#x}")
    expected = compute_footer(data[:-FOOTER_SIZE])
    stored = bytes(data[-FOOTER_SIZE:])

    if args.check:
        if stored != expected:
            raise SystemExit(
                f"FAIL: stored {stored.hex()}, expected {expected.hex()}"
            )
        print(f"PASS: {args.input} footer {stored.hex()}")
        return 0

    if args.output is None:
        parser.error("output is required unless --check is used")
    data[-FOOTER_SIZE:] = expected
    args.output.write_bytes(data)
    changed = sum(a != b for a, b in zip(stored, expected))
    print(
        f"wrote {args.output}: footer {stored.hex()} -> {expected.hex()} "
        f"({changed} byte(s) changed)"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
