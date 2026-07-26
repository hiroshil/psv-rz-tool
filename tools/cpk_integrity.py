#!/usr/bin/env python3
"""Regenerate the 16-byte fixed-sector integrity footer in target game CPK entries.

The engine's common CPK reader calls FUN_8102B4AC after each fixed-sector read.
The callback excludes the final 16 bytes, sums alternating little-endian u64
lanes with seed 0x1111111111111111, and compares those sums with the footer.

This utility is intended for addpt.cpk, bk.cpk, bsf.cpk, pt.cpk and sc.cpk
rebuilt by older rz-tool versions that preserved a stale footer after edits.
"""
from __future__ import annotations

import argparse
import os
import sys
from pathlib import Path

from sc_integrity import enumerate_entries, sc_footer

FOOTER_SIZE = 0x10


def repair(input_path: Path, output_path: Path, check_only: bool) -> int:
    source = input_path.read_bytes()
    archive = bytearray(source)
    entries = enumerate_entries(source)
    changed: list[int] = []
    for entry in entries:
        footer_offset = entry.offset + entry.size - FOOTER_SIZE
        expected = sc_footer(source[entry.offset:footer_offset])
        stored = source[footer_offset : footer_offset + FOOTER_SIZE]
        if stored != expected:
            changed.append(entry.entry_id)
            archive[footer_offset : footer_offset + FOOTER_SIZE] = expected

    if check_only:
        if changed:
            print(
                f"CPK integrity mismatch in {len(changed)} entries: "
                + ", ".join(map(str, changed))
            )
            return 1
        print(f"CPK integrity OK: {len(entries)} entries")
        return 0

    if input_path.resolve() == output_path.resolve():
        raise ValueError("input and output paths must differ")
    temporary = output_path.with_name(output_path.name + ".tmp")
    temporary.write_bytes(archive)
    os.replace(temporary, output_path)
    print(
        f"wrote {output_path} "
        f"({len(entries)} entries, regenerated {len(changed)} footers)"
    )
    if changed:
        print("changed entry IDs: " + ", ".join(map(str, changed)))
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("input", type=Path)
    parser.add_argument("output", type=Path, nargs="?")
    parser.add_argument("--check", action="store_true", help="verify only; do not write output")
    args = parser.parse_args()
    if args.check:
        if args.output is not None:
            parser.error("output is not accepted with --check")
        output = args.input
    else:
        if args.output is None:
            parser.error("output is required unless --check is used")
        output = args.output
    try:
        return repair(args.input, output, args.check)
    except (OSError, ValueError, KeyError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
