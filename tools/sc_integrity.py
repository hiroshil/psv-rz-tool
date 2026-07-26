#!/usr/bin/env python3
"""Regenerate the 16-byte integrity footer for every entry in this game's sc.cpk.

The algorithm is the exact behavior of eboot.bin.elf FUN_8102B4AC:
- exclude the final 16 bytes of each fixed-size entry allocation;
- process the remaining bytes in 16-byte blocks;
- add the first u64 of every block to lane 0 and the second u64 to lane 1;
- seed both lanes with 0x1111111111111111;
- store lane 0 and lane 1 little-endian in the final 16 bytes.
"""

from __future__ import annotations

import argparse
import os
import struct
import sys
from dataclasses import dataclass
from pathlib import Path

SEED = 0x1111111111111111
MASK64 = (1 << 64) - 1
FOOTER_SIZE = 0x10


def decrypt_utf_table(data: bytes) -> bytes:
    output = bytearray(data)
    key = 0x5F
    for index in range(len(output)):
        output[index] ^= key
        key = (key * 0x15) & 0xFF
    return bytes(output)


def c_string(pool: bytes, offset: int) -> str:
    if offset == 0xFFFFFFFF:
        return ""
    end = pool.find(b"\0", offset)
    if end < 0:
        raise ValueError("unterminated @UTF string")
    raw = pool[offset:end]
    for encoding in ("utf-8", "shift_jis"):
        try:
            return raw.decode(encoding)
        except UnicodeDecodeError:
            pass
    raise ValueError("unsupported @UTF string encoding")


TYPE_SIZES = {0: 1, 1: 1, 2: 2, 3: 2, 4: 4, 5: 4, 6: 8, 7: 8, 8: 4, 9: 8, 10: 4, 11: 8, 12: 16}


def read_value(data: bytes, position: int, value_type: int):
    if value_type not in TYPE_SIZES:
        raise ValueError(f"unsupported @UTF type {value_type}")
    size = TYPE_SIZES[value_type]
    raw = data[position : position + size]
    if len(raw) != size:
        raise ValueError("truncated @UTF value")
    if value_type == 0:
        return raw[0]
    if value_type == 1:
        return struct.unpack(">b", raw)[0]
    if value_type == 2:
        return struct.unpack(">H", raw)[0]
    if value_type == 3:
        return struct.unpack(">h", raw)[0]
    if value_type == 4:
        return struct.unpack(">I", raw)[0]
    if value_type == 5:
        return struct.unpack(">i", raw)[0]
    if value_type == 6:
        return struct.unpack(">Q", raw)[0]
    if value_type == 7:
        return struct.unpack(">q", raw)[0]
    if value_type == 8:
        return struct.unpack(">f", raw)[0]
    if value_type == 9:
        return struct.unpack(">d", raw)[0]
    if value_type == 10:
        return ("string", struct.unpack(">I", raw)[0])
    if value_type == 11:
        offset, length = struct.unpack(">II", raw)
        return ("data", offset, length)
    return raw


def parse_utf(data: bytes) -> list[dict[str, object]]:
    if not data.startswith(b"@UTF") or len(data) < 0x20:
        raise ValueError("invalid @UTF table")
    total_size = struct.unpack_from(">I", data, 4)[0] + 8
    if total_size > len(data):
        raise ValueError("truncated @UTF table")
    data = data[:total_size]
    rows_offset = struct.unpack_from(">H", data, 10)[0] + 8
    strings_offset = struct.unpack_from(">I", data, 12)[0] + 8
    data_offset = struct.unpack_from(">I", data, 16)[0] + 8
    column_count = struct.unpack_from(">H", data, 24)[0]
    row_size = struct.unpack_from(">H", data, 26)[0]
    row_count = struct.unpack_from(">I", data, 28)[0]
    if not (0x20 <= rows_offset <= strings_offset <= data_offset <= total_size):
        raise ValueError("invalid @UTF section offsets")
    string_pool = data[strings_offset:data_offset]

    position = 0x20
    schema: list[tuple[str, int, int, object | None]] = []
    for _ in range(column_count):
        flags = data[position]
        position += 1
        value_type = flags & 0x0F
        name_offset = 0xFFFFFFFF
        if flags & 0x10:
            name_offset = struct.unpack_from(">I", data, position)[0]
            position += 4
        default = None
        if flags & 0x20:
            default = read_value(data, position, value_type)
            position += TYPE_SIZES[value_type]
        schema.append((c_string(string_pool, name_offset), flags, value_type, default))

    rows: list[dict[str, object]] = []
    for row_index in range(row_count):
        position = rows_offset + row_index * row_size
        row: dict[str, object] = {}
        for name, flags, value_type, default in schema:
            if flags & 0x40:
                value = read_value(data, position, value_type)
                position += TYPE_SIZES[value_type]
            else:
                value = default
            if isinstance(value, tuple) and value[0] == "string":
                value = c_string(string_pool, value[1])
            elif isinstance(value, tuple) and value[0] == "data":
                _, offset, length = value
                start = data_offset + offset
                end = start + length
                if end > len(data):
                    raise ValueError("truncated @UTF data value")
                value = data[start:end]
            row[name] = value
        rows.append(row)
    return rows


def parse_chunk(archive: bytes, offset: int, magic: bytes) -> list[dict[str, object]]:
    if archive[offset : offset + 4] != magic:
        raise ValueError(f"missing {magic.decode('ascii')} chunk at {offset:#x}")
    table_size = struct.unpack_from("<Q", archive, offset + 8)[0]
    start = offset + 0x10
    end = start + table_size
    if end > len(archive):
        raise ValueError(f"truncated {magic.decode('ascii')} chunk")
    table = archive[start:end]
    if not table.startswith(b"@UTF"):
        table = decrypt_utf_table(table)
    return parse_utf(table)


@dataclass(frozen=True)
class Entry:
    entry_id: int
    size: int
    offset: int


def align_up(value: int, alignment: int) -> int:
    return (value + alignment - 1) // alignment * alignment


def enumerate_entries(archive: bytes) -> list[Entry]:
    header_rows = parse_chunk(archive, 0, b"CPK ")
    if len(header_rows) != 1:
        raise ValueError("unexpected CPK header row count")
    header = header_rows[0]
    content_offset = int(header["ContentOffset"])
    itoc_offset = int(header["ItocOffset"])
    alignment = int(header["Align"])
    expected_files = int(header["Files"])
    if int(header.get("EnableFileCrc") or 0) != 0:
        raise ValueError("this fixer does not rewrite CPK FileCrc tables")

    itoc_rows = parse_chunk(archive, itoc_offset, b"ITOC")
    if len(itoc_rows) != 1:
        raise ValueError("unexpected ITOC row count")
    itoc = itoc_rows[0]
    low = parse_utf(bytes(itoc["DataL"]))
    high = parse_utf(bytes(itoc["DataH"]))
    rows = sorted(low + high, key=lambda row: int(row["ID"]))
    if len(rows) != expected_files:
        raise ValueError(f"ITOC has {len(rows)} files, CPK header declares {expected_files}")
    if len({int(row["ID"]) for row in rows}) != len(rows):
        raise ValueError("duplicate ITOC entry ID")

    result: list[Entry] = []
    position = content_offset
    for row in rows:
        entry_id = int(row["ID"])
        file_size = int(row["FileSize"])
        extract_size = int(row["ExtractSize"])
        if file_size != extract_size:
            raise ValueError(
                f"entry {entry_id} is compressed ({file_size:#x} != {extract_size:#x}); not an SC allocation"
            )
        if file_size < FOOTER_SIZE or file_size % 0x10 != 0:
            raise ValueError(f"entry {entry_id} has invalid SC allocation size {file_size:#x}")
        if position + file_size > len(archive):
            raise ValueError(f"entry {entry_id} exceeds archive bounds")
        result.append(Entry(entry_id, file_size, position))
        position += align_up(file_size, alignment)
    return result


def sc_footer(entry_without_footer: bytes) -> bytes:
    if len(entry_without_footer) % 0x10 != 0:
        raise ValueError("SC checksum domain is not 16-byte aligned")
    lane_0 = SEED
    lane_1 = SEED
    for position in range(0, len(entry_without_footer), 0x10):
        lane_0 = (lane_0 + int.from_bytes(entry_without_footer[position : position + 8], "little")) & MASK64
        lane_1 = (lane_1 + int.from_bytes(entry_without_footer[position + 8 : position + 0x10], "little")) & MASK64
    return lane_0.to_bytes(8, "little") + lane_1.to_bytes(8, "little")


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
            print(f"SC integrity mismatch in {len(changed)} entries: " + ", ".join(map(str, changed)))
            return 1
        print(f"SC integrity OK: {len(entries)} entries")
        return 0

    if input_path.resolve() == output_path.resolve():
        raise ValueError("input and output paths must differ")
    temporary = output_path.with_name(output_path.name + ".tmp")
    temporary.write_bytes(archive)
    os.replace(temporary, output_path)
    print(f"wrote {output_path} ({len(entries)} entries, regenerated {len(changed)} footers)")
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
    except (OSError, ValueError, KeyError, struct.error) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
