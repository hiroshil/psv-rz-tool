#!/usr/bin/env python3
"""Reproducible reverse-engineering evidence for rz-tool 1.0.1.

The report is generated only from PCSG00933 eboot.bin.elf and mapper.json.
It deliberately separates directly observed instructions/static data from the
conservative implementation decisions derived from them.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import struct
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable, Iterator

try:
    from capstone import CS_ARCH_ARM, CS_MODE_LITTLE_ENDIAN, CS_MODE_THUMB, Cs
except ImportError as error:  # pragma: no cover
    raise SystemExit("Capstone is required") from error

PT_LOAD = 1
REGISTRY_ADDRESS = 0x8112A2E0
REGISTRY_RECORD_SIZE = 0x1C
REGISTRY_RECORDS = 10
PR_OFFSET_TABLE = 0x81113308
PR_SLOT_COUNT = 0x23
STANDALONE_TABLE = 0x81129E58
STANDALONE_RECORD_SIZE = 0x10
STANDALONE_RECORDS = 3

SECTOR_TABLES = (
    ("addpt.cpk", 0x811122DC, 1, 4, 2),
    ("pt.cpk", 0x81113394, 0x17, 4, 2),
    ("sc.cpk", 0x8111344C, 0x59, 4, 2),
    ("bk.cpk", 0x810FA500, 0x146, 8, 4),
    ("bsf.cpk", 0x810FAF30, 0x126, 8, 4),
)

RUNTIME_DESCRIPTORS = (
    ("stream-disabled", 0x81129EF0),
    ("bk-stream", 0x81129F20),
    ("bsf-group-0", 0x81129F50),
    ("bsf-group-1", 0x81129F80),
    ("bsf-group-2", 0x81129FB0),
    ("bsf-group-3", 0x81129FE0),
    ("bk-loader-a", 0x8112B8A8),
    ("bk-loader-b", 0x8112B8D8),
)

FUNCTIONS = {
    "registry": 0x810224BC,
    "resource_allocations": 0x8101B504,
    "standalone_loader": 0x8101A9C0,
    "entry_reader": 0x81053B7A,
    "package_loader": 0x81053694,
    "gzip_block": 0x81034E4A,
    "metadata_installer": 0x810225FA,
    "bundle_loader": 0x81053FA6,
    "stream_setup": 0x8101D4C8,
    "bsf_setup": 0x8101FDE8,
    "bsf_step": 0x8101FEA6,
    "bk_loader": 0x81053CDA,
    "addpt_loader": 0x8104D652,
    "pt_loader": 0x8105403E,
    "pt_first_package": 0x81054260,
    "texture_copy": 0x8102FDCA,
    "palette_copy": 0x8102FD82,
    "pr_lookup": 0x81053670,
    "pr_direct_texture": 0x8100AE84,
    "pr_compressed_atlas": 0x8103EF36,
    "lt_renderer": 0x8102D194,
    "lt_wrapper": 0x8102D78C,
    "lt_string_raster": 0x8104DB86,
    "script_load": 0x81053CB6,
    "script_reload": 0x81019B6C,
    "script_layout": 0x8101A6EA,
    "script_voice_init": 0x8101666C,
    "script_global_flag": 0x8101C516,
    "script_global_secondary": 0x8101C6C4,
    "script_init": 0x81000512,
    "script_lookup": 0x810003B2,
    "script_interpreter": 0x8100132C,
    "script_text_entry": 0x81006A86,
    "script_text_parser": 0x8104DF94,
    "script_text_wrapper": 0x8104E0A6,
    "script_layer_image": 0x81008300,
    "script_bk_request": 0x81017744,
    "script_bk_commit": 0x8101781E,
    "itoc_search": 0x81066E32,
    "itoc_prefix_sum": 0x81066ED2,
    "itoc_lookup": 0x8106706C,
}

SELECTED = {
    "registry": {0x810224EC, 0x810224F2, 0x810224FC, 0x810224FE},
    "resource_allocations": {
        0x8101B510, 0x8101B514, 0x8101B528, 0x8101B52C,
        0x8101B540, 0x8101B554, 0x8101B568,
    },
    "entry_reader": {
        0x81053B8A, 0x81053B8C, 0x81053B8E,
        0x81053C7E, 0x81053C80, 0x81053C82,
    },
    "package_loader": {
        0x8105369C, 0x810536A4, 0x810536AC, 0x810536AE,
        0x810536B2, 0x810536B6, 0x810536BA, 0x810536D0,
        0x810536D4, 0x810536DA, 0x810536DC, 0x810536EA,
        0x810536F0, 0x810536F2, 0x810536F4, 0x810536F6,
        0x81053734, 0x81053742, 0x81053746, 0x81053748,
        0x8105374A, 0x8105374C, 0x81053764, 0x81053768,
        0x8105376E,
    },
    "gzip_block": {0x81034E5A, 0x81034E5E, 0x81034E66},
    "metadata_installer": {
        0x8102260A, 0x8102260C, 0x81022654, 0x81022656,
        0x81022672, 0x81022684, 0x81022686, 0x81022688,
    },
    "bundle_loader": {
        0x81053FB0, 0x81053FBE, 0x81053FC2, 0x81053FC4,
        0x81053FDA, 0x81053FDC, 0x81053FDE, 0x81053FE6,
    },
    "stream_setup": {
        0x8101D4D4, 0x8101D4FC, 0x8101D504, 0x8101D508,
        0x8101D526, 0x8101D528, 0x8101D532, 0x8101D536,
        0x8101D544, 0x8101D550, 0x8101D554, 0x8101D55A,
        0x8101D562, 0x8101D584, 0x8101D592, 0x8101D596,
        0x8101D5A0, 0x8101D5A8,
    },
    "bsf_setup": {
        0x8101FE58, 0x8101FE60, 0x8101FE64, 0x8101FE6C,
        0x8101FE6E, 0x8101FE74, 0x8101FE78, 0x8101FE7A,
        0x8101FE84, 0x8101FE8A, 0x8101FE8E,
    },
    "bsf_step": {
        0x8101FF08, 0x8101FF10, 0x8101FF18, 0x8101FF20,
        0x8101FF22, 0x8101FF26, 0x8101FF2A, 0x8101FF2C,
        0x8101FF30, 0x8101FF32, 0x8101FF36, 0x8101FF3E,
        0x8101FF40, 0x8101FF52, 0x8101FF5E, 0x8101FF68,
        0x8101FF6E, 0x8101FF78, 0x8101FF7C, 0x8101FF7E,
        0x8101FF9E,
    },
    "bk_loader": {
        0x81053D30, 0x81053D34, 0x81053D42,
        0x81053DA0, 0x81053DA2, 0x81053DAE,
        0x81053DD2, 0x81053DD6, 0x81053DDA, 0x81053DDC,
        0x81053DF2, 0x81053DF4, 0x81053DFC,
    },
    "addpt_loader": {
        0x8104D6E0, 0x8104D6E6, 0x8104D6EA, 0x8104D6EE,
        0x8104D6F4, 0x8104D6F6, 0x8104D71A, 0x8104D726,
        0x8104D728,
    },
    "pt_loader": {
        0x8105412A, 0x8105412E, 0x81054132, 0x81054136,
        0x8105413C, 0x8105413E, 0x81054184, 0x8105418E,
    },
    "texture_copy": {
        0x8102FE08, 0x8102FE0C, 0x8102FE18, 0x8102FE1C,
        0x8102FE24, 0x8102FE28, 0x8102FE2C, 0x8102FE52,
        0x8102FE56, 0x8102FE8A, 0x8102FE8E, 0x8102FE90,
    },
    "palette_copy": {
        0x8102FDAC, 0x8102FDAE, 0x8102FDB2, 0x8102FDB6,
        0x8102FDBC, 0x8102FDC4,
    },
    "pr_lookup": {
        0x81053670, 0x81053672, 0x8105367A, 0x81053682,
        0x81053686, 0x8105368E, 0x81053690,
    },
    "pr_direct_texture": {
        0x8100AEAA, 0x8100AEAC, 0x8100AEB4, 0x8100AEBE,
        0x8100AEC4, 0x8100AEC6, 0x8100AECE,
    },
    "pr_compressed_atlas": {
        0x8103EF70, 0x8103EF72, 0x8103EF82,
        0x8103EF94, 0x8103EF96, 0x8103EFA6,
        0x8103EFB8, 0x8103EFBA, 0x8103EFCA,
        0x8103EFD4, 0x8103EFD6, 0x8103EFDC,
        0x8103EFDE, 0x8103EFE0, 0x8103EFEE, 0x8103EFF2,
    },
    "lt_renderer": {
        0x8102D37C, 0x8102D380, 0x8102D5E8, 0x8102D5FA,
        0x8102D600, 0x8102D606, 0x8102D614, 0x8102D61A,
        0x8102D62E, 0x8102D632, 0x8102D636, 0x8102D638,
    },
    "script_load": {0x81053CB8, 0x81053CBC, 0x81053CC4, 0x81053CCC, 0x81053CD0, 0x81053CD2, 0x81053CD4},
    "script_reload": {0x81019B8A, 0x81019B96, 0x81019B9C, 0x81019BA0, 0x81019BA4},
    "script_layout": {0x8101A788, 0x8101A790, 0x8101A79A, 0x8101A79E, 0x8101A7A8, 0x8101A7AC},
    "script_voice_init": {0x810166A2, 0x810166A4, 0x810166B4, 0x810166B6, 0x810166BE, 0x810166C6, 0x810166C8},
    "script_global_flag": {0x8101C528, 0x8101C52A, 0x8101C538, 0x8101C53A, 0x8101C53C, 0x8101C540, 0x8101C542},
    "script_global_secondary": {0x8101C6DA, 0x8101C6E8, 0x8101C6EC, 0x8101C6EE, 0x8101C6F2, 0x8101C6F4},
    "script_init": {0x8100052E, 0x81000532, 0x81000536, 0x81000550, 0x81000552},
    "script_lookup": {0x810003B4, 0x810003C6, 0x810003CA, 0x810003CC, 0x810003D0},
    "script_interpreter": {
        0x8100140E, 0x81001412, 0x8100141A, 0x8100141E,
        0x81001422, 0x81001426, 0x8100142A, 0x8100142E,
        0x810021B8, 0x810021BE, 0x810021E2, 0x810021E8,
        0x810021EE, 0x810021F0, 0x810021F2,
    },
    "script_text_entry": {
        0x81006A88, 0x81006A8C, 0x81006A90, 0x81006A94,
        0x81006A9A, 0x81006A9E, 0x81006AAA,
    },
    "script_text_parser": {
        0x8104DFB6, 0x8104DFBC, 0x8104DFC4, 0x8104DFC6,
        0x8104DFE8, 0x8104DFEA, 0x8104DFEC, 0x8104E048,
        0x8104E052, 0x8104E054, 0x8104E084, 0x8104E088,
    },
    "script_text_wrapper": {
        0x8104E0AA, 0x8104E0AE, 0x8104E0B4, 0x8104E0BE,
    },
    "script_layer_image": {0x81008306, 0x8100830E, 0x81008312, 0x810084B4},
    "script_bk_request": {
        0x81017786, 0x8101778C, 0x81017790, 0x81017794,
        0x81017798, 0x810177B0, 0x810177BE, 0x810177C0,
    },
    "script_bk_commit": {0x81017830, 0x8101783A, 0x81017840},
    "itoc_search": {
        0x81066E3E, 0x81066E5A, 0x81066E62,
        0x81066ECA, 0x81066ECC,
    },
    "itoc_prefix_sum": {
        0x81067016, 0x8106701E, 0x81067022, 0x81067026,
        0x8106702E, 0x81067040, 0x81067048, 0x8106704C,
        0x81067050, 0x81067058,
    },
    "itoc_lookup": {
        0x81067072, 0x81067076, 0x8106707C,
        0x81067082, 0x81067086, 0x81067088,
        0x81067094, 0x81067096, 0x8106709C,
        0x810670A0, 0x810670A8,
        0x810670F8, 0x810670FA, 0x81067100,
        0x81067104, 0x8106710C,
    },
}

NOTES = {
    "entry_reader": "The requested byte count is sector_count << 11 (0x800 bytes per sector).",
    "package_loader": "Shared image loader: caller-selected table delta; byte active index; full-u32 count; index*stride destination; optional descriptor/metadata/palette install. The body runs before the comparison, so stored counts 0 and 1 both execute exactly chunk 0.",
    "gzip_block": "Compressed block starts with output size and three opaque u32 words; RFC1952 stream starts at +0x10.",
    "metadata_installer": "Metadata records are 0x20 bytes and the per-bank total must remain strictly below 0x400 records.",
    "bundle_loader": "Outer package count is one byte and each logical package offset is independently indexed; neighboring offsets are never compared here.",
    "stream_setup": "Six streaming groups: BK uses archive 4/326 entries/0x220000 output; BSF uses archive 5 split 154 + 140 entries with 0x320000/0x300000 outputs.",
    "bsf_setup": "BSF copies 0x830 bytes from chunk_base and selects the active table at chunk_base + 0x1400.",
    "bsf_step": "BSF truncates the table count to its low byte, writes chunk i to output+i*stride, then copies descriptor-height rows. It also executes chunk 0 before comparing, so low-byte counts 0 and 1 are both one-chunk encodings.",
    "bk_loader": "BK uses archive 4, table delta 0x1400, NULL descriptor output, and a separately allocated 1024x544 runtime surface.",
    "addpt_loader": "ADDPT reads its executable sector record then invokes the outer bundle loader.",
    "pt_loader": "PT reads its per-ID executable sector record then invokes the outer bundle loader.",
    "texture_copy": "Zero region arguments select full runtime dimensions; RGBA8 copies width*height*4 bytes.",
    "palette_copy": "Palette copy is performed only for runtime palette sizes 0x40 or 0x400.",
    "lt_renderer": "24px glyph path bounds IDs to <0xE12 and addresses glyph_id*0x120, 12 packed bytes for each of 24 rows.",
    "script_load": "SC entry IDs below 0x59 are read from archive index 1 through FUN_81053B7A using the executable sector table at 0x8111344C. This proves that compiled script payloads are external in sc.cpk, not embedded wholesale in the ELF.",
    "script_reload": "Scene changes load the selected SC entry into DAT_811B98F0 and then reinitialize the script VM.",
    "script_layout": "The loaded SC entry is split into voice header at +0x00, generated runtime tables at +0x80, and stream table/compiled payload at +0x2000.",
    "script_voice_init": "The first 16 bytes are compared with voice_not_exist; otherwise the first u16 is installed as the local voice base.",
    "script_global_flag": "The first base/count pair in each 0x0C-byte record at 0x810F9B1C maps a local index into a global runtime bitset.",
    "script_global_secondary": "The third base/count pair at record +0x08/+0x0A maps the local secondary domain into a separate global bitset. The ELF table is metadata, not script payload.",
    "script_init": "Initialization installs the two tables at entry+0x80 and sets script_base to entry+0x2000. The first u32 at script_base is both stream-0 offset and the dense offset-table byte extent.",
    "script_lookup": "Compiled-script stream lookup is script_base + offsets[u16 id], with no monotonicity check.",
    "script_interpreter": "When executed as bytecode, words below 0xE12 are glyph IDs and selected 0xFFxx values are controls. The FFF0 case passes the current marker to FUN_81006A86 and advances by the parser's returned u16 count plus the marker/index words.",
    "script_text_entry": "Selects one 0x1C-byte text-render state and delegates to FUN_8104E0A6.",
    "script_text_parser": "Consumes the marker/index prefix, scans speaker glyphs until 0xFFFF, returns the number of consumed u16 words, and hands the terminated speaker string to the renderer.",
    "script_text_wrapper": "Clears the render buffer and invokes the speaker parser; paired with the FFFE interpreter case and corpus validation this establishes the exported speaker/dialogue grammar.",
    "script_layer_image": "Opcode 0xFF33 passes a layer byte and image ID into FUN_81008300, which reaches the BK/BSF streaming dispatcher FUN_81020316.",
    "script_bk_request": "Opcode 0xFF49 validates a BK ID below 0x146 and requests archive class 1 through FUN_81053E80; SC stores the reference, not the texture bytes.",
    "script_bk_commit": "Opcode 0xFF48 advances the BK loader FUN_81053CDA, which decodes from bk.cpk into the runtime texture.",
    "itoc_search": "Binary search returns either a row index or a negative insertion-position encoding.",
    "itoc_prefix_sum": "The offset routine independently sums aligned prefixes from both DataL and DataH; this is the physical merge-by-ID proof.",
    "itoc_lookup": "The lookup searches both tables, converts the missing table result to an insertion index, and passes both prefix counts into FUN_81066ED2.",
}


@dataclass(frozen=True)
class Segment:
    virtual_address: int
    file_offset: int
    file_size: int


class ElfImage:
    def __init__(self, path: Path) -> None:
        self.data = path.read_bytes()
        if self.data[:4] != b"\x7fELF" or self.data[4] != 1 or self.data[5] != 1:
            raise ValueError("expected little-endian ELF32")
        program_offset = struct.unpack_from("<I", self.data, 0x1C)[0]
        entry_size = struct.unpack_from("<H", self.data, 0x2A)[0]
        count = struct.unpack_from("<H", self.data, 0x2C)[0]
        self.segments: list[Segment] = []
        for index in range(count):
            offset = program_offset + index * entry_size
            kind, file_offset, virtual_address, _, file_size = struct.unpack_from(
                "<IIIII", self.data, offset
            )
            if kind == PT_LOAD and file_size:
                self.segments.append(Segment(virtual_address, file_offset, file_size))
        if not self.segments:
            raise ValueError("ELF has no PT_LOAD segment")

    def read(self, address: int, size: int) -> bytes:
        for segment in self.segments:
            relative = address - segment.virtual_address
            if 0 <= relative and relative + size <= segment.file_size:
                start = segment.file_offset + relative
                return self.data[start : start + size]
        raise ValueError(f"unmapped virtual range {address:#x}+{size:#x}")


class FunctionMap:
    def __init__(self, path: Path) -> None:
        document = json.loads(path.read_text(encoding="utf-8"))
        functions = document.get("functions", document)
        self.entries = sorted(
            (int(item["entry_address"], 16) & ~1, name)
            for name, item in functions.items()
        )
        self.by_address = {address: name for address, name in self.entries}

    def bounds(self, address: int) -> tuple[int, int, str]:
        address &= ~1
        for index, (start, name) in enumerate(self.entries[:-1]):
            if start == address:
                return start, self.entries[index + 1][0], name
        raise ValueError(f"function {address:#x} absent from mapper")

    def all_bounds(self) -> Iterator[tuple[int, int, str]]:
        for index, (start, name) in enumerate(self.entries[:-1]):
            yield start, self.entries[index + 1][0], name


def disassemble(image: ElfImage, functions: FunctionMap, address: int):
    start, end, name = functions.bounds(address)
    md = Cs(CS_ARCH_ARM, CS_MODE_THUMB | CS_MODE_LITTLE_ENDIAN)
    return name, list(md.disasm(image.read(start, end - start), start))


def print_selected(instructions: Iterable, addresses: set[int]) -> None:
    found = set()
    for instruction in instructions:
        if instruction.address in addresses:
            found.add(instruction.address)
            print(f"{instruction.address:08x}: {instruction.mnemonic:10s} {instruction.op_str}")
    missing = addresses - found
    if missing:
        raise ValueError(f"selected addresses not decoded: {sorted(hex(x) for x in missing)}")


def report_function(image: ElfImage, functions: FunctionMap, key: str) -> None:
    address = FUNCTIONS[key]
    name, instructions = disassemble(image, functions, address)
    print(f"[{key}: {name} @ {address:#010x}]")
    print(NOTES.get(key, "Selected routine evidence."))
    print_selected(instructions, SELECTED[key])
    print()


def decode_c_string(field: bytes) -> str:
    return field.split(b"\0", 1)[0].decode("ascii", errors="replace")


def read_sector_table(image: ElfImage, address: int, count: int, stride: int, field: int) -> list[int]:
    return [struct.unpack("<H", image.read(address + index * stride + field, 2))[0] for index in range(count)]


def report_static_tables(image: ElfImage) -> None:
    print(f"[resource records @ {REGISTRY_ADDRESS:#010x}]")
    for index in range(REGISTRY_RECORDS):
        address = REGISTRY_ADDRESS + index * REGISTRY_RECORD_SIZE
        record = image.read(address, REGISTRY_RECORD_SIZE)
        print(f"{index:2d}: {decode_c_string(record[:0x10]):<10s} count={struct.unpack_from('<I', record, 0x10)[0]:#x}")
    print()

    print(f"[standalone resource records @ {STANDALONE_TABLE:#010x}]")
    for index in range(STANDALONE_RECORDS):
        address = STANDALONE_TABLE + index * STANDALONE_RECORD_SIZE
        resource_id, byte_size, sectors, destination = struct.unpack("<4I", image.read(address, 0x10))
        print(f"{index}: id={resource_id} size={byte_size:#x} sectors={sectors:#x} loaded={sectors*0x800:#x} destination={destination:#010x}")
    print()

    print("[executable sector tables]")
    for name, address, count, stride, field in SECTOR_TABLES:
        values = read_sector_table(image, address, count, stride, field)
        digest = hashlib.sha256(struct.pack(f"<{len(values)}H", *values)).hexdigest()
        print(f"{name}: address={address:#010x} records={count} stride={stride} sector_field=+{field} sha256={digest}")
        for start in range(0, len(values), 16):
            print("  " + " ".join(str(value) for value in values[start:start+16]))
    print()

    print("[SC global metadata table @ 0x810f9b1c]")
    sc_metadata = image.read(0x810F9B1C, 0x59 * 0x0C)
    for index in range(0x59):
        stream_base, stream_count, primary_base, primary_count, secondary_base, secondary_count = struct.unpack_from("<6H", sc_metadata, index * 0x0C)
        print(f"{index:02d}: streams={stream_base}+{stream_count} primary={primary_base}+{primary_count} secondary={secondary_base}+{secondary_count}")
    print("These records provide global bases and expected local counts; FUN_81053CB6 still loads every actual script payload from sc.cpk.")
    print()

    print("[runtime texture descriptors]")
    for label, address in RUNTIME_DESCRIPTORS:
        descriptor = image.read(address, 0x30)
        size = struct.unpack_from("<I", descriptor, 0x04)[0]
        pointer = struct.unpack_from("<I", descriptor, 0x10)[0]
        palette = struct.unpack_from("<I", descriptor, 0x14)[0]
        texture_type = struct.unpack_from("<I", descriptor, 0x1C)[0]
        texture_format = struct.unpack_from("<I", descriptor, 0x20)[0]
        width, height = struct.unpack_from("<HH", descriptor, 0x28)
        print(f"{label:15s} {address:#010x}: size={size:#x} ptr={pointer:#x} palette={palette:#x} type={texture_type:#010x} format={texture_format:#010x} {width}x{height}")
    print()

    values = struct.unpack(f"<{PR_SLOT_COUNT}I", image.read(PR_OFFSET_TABLE, PR_SLOT_COUNT * 4))
    print(f"[pr.bin slot offsets @ {PR_OFFSET_TABLE:#010x}]")
    for index, value in enumerate(values):
        print(f"{index:2d}: {value:#010x}")
    print()


def parse_direct_target(op_str: str) -> int | None:
    if not op_str.startswith("#0x"):
        return None
    try:
        return int(op_str[1:], 16) & ~1
    except ValueError:
        return None


def report_call_census(image: ElfImage, functions: FunctionMap) -> None:
    targets = {
        FUNCTIONS["entry_reader"]: "entry_reader",
        FUNCTIONS["package_loader"]: "package_loader",
        FUNCTIONS["bundle_loader"]: "bundle_loader",
        FUNCTIONS["metadata_installer"]: "metadata_installer",
        FUNCTIONS["texture_copy"]: "texture_copy",
        FUNCTIONS["palette_copy"]: "palette_copy",
        FUNCTIONS["lt_renderer"]: "lt_renderer",
        FUNCTIONS["lt_wrapper"]: "lt_wrapper",
        FUNCTIONS["script_lookup"]: "script_lookup",
        FUNCTIONS["bsf_step"]: "bsf_step",
    }
    calls: dict[int, list[tuple[int, str]]] = {target: [] for target in targets}
    md = Cs(CS_ARCH_ARM, CS_MODE_THUMB | CS_MODE_LITTLE_ENDIAN)
    for start, end, name in functions.all_bounds():
        try:
            code = image.read(start, end - start)
        except ValueError:
            continue
        for instruction in md.disasm(code, start):
            if instruction.mnemonic != "bl":
                continue
            target = parse_direct_target(instruction.op_str)
            if target in calls:
                calls[target].append((instruction.address, name))
    print("[direct caller census]")
    for target, label in targets.items():
        print(f"{label} {target:#010x}:")
        for address, caller in calls[target]:
            print(f"  {address:#010x} {caller}")
    print()



def materialized_address_functions(
    image: ElfImage, functions: FunctionMap, target: int
) -> list[tuple[int, str, int, int]]:
    """Find MOVW/MOVT pairs that materialize an absolute address.

    This is intentionally narrower than a general data-flow analysis, but for
    this ELF's compiler output it provides a reproducible whole-function census
    of direct absolute references. A pending MOVW is invalidated when the same
    destination register is overwritten before MOVT.
    """
    import re

    low = target & 0xFFFF
    high = target >> 16
    immediate = re.compile(r"^(r(?:1[0-2]|[0-9])|sl|sb|fp|ip|lr), #0x([0-9a-f]+)$")
    results: list[tuple[int, str, int, int]] = []
    md = Cs(CS_ARCH_ARM, CS_MODE_THUMB | CS_MODE_LITTLE_ENDIAN)
    for start, end, name in functions.all_bounds():
        try:
            instructions = list(md.disasm(image.read(start, end - start), start))
        except ValueError:
            continue
        pending: dict[str, int] = {}
        for instruction in instructions:
            match = immediate.match(instruction.op_str)
            destination = instruction.op_str.split(",", 1)[0].strip()
            if instruction.mnemonic == "movw" and match:
                if int(match.group(2), 16) == low:
                    pending[match.group(1)] = instruction.address
                else:
                    pending.pop(match.group(1), None)
                continue
            if instruction.mnemonic == "movt" and match:
                register = match.group(1)
                movw_address = pending.pop(register, None)
                if movw_address is not None and int(match.group(2), 16) == high:
                    results.append((start, name, movw_address, instruction.address))
                continue
            if destination in pending:
                pending.pop(destination, None)
    return results


def report_absolute_reference_census(image: ElfImage, functions: FunctionMap) -> None:
    targets = (
        (0x811B98E4, "lt.bin base pointer global"),
        (0x8110C46C, "glyph renderer dispatch table base"),
    )
    print("[absolute-address materialization census]")
    for target, label in targets:
        print(f"{label} {target:#010x}:")
        for start, name, movw_address, movt_address in materialized_address_functions(
            image, functions, target
        ):
            print(
                f"  function={start:#010x} {name} "
                f"movw={movw_address:#010x} movt={movt_address:#010x}"
            )
    print("The lt.bin list exhausts direct MOVW/MOVT materializations in mapped code. Manual data-flow audit classifies FUN_8101b504 as allocation, FUN_8101a9c0 as loading, and the remaining references as glyph-renderer or string-raster paths using the same 24px family.")
    print()

def report_function_pointer_tables(image: ElfImage) -> None:
    values = struct.unpack("<4I", image.read(0x8110C470, 16))
    print("[lt renderer function-pointer table @ 0x8110c470]")
    for index, value in enumerate(values):
        print(f"{index}: {value:#010x} -> Thumb target {(value & ~1):#010x}")
    print("Entries 0 and 1 point directly to FUN_8102d78c and FUN_8102d194; located string-raster paths pass DAT_811b98e4 to the same 24px renderer family.")
    print()


def report_derived_limits() -> None:
    glyph_bytes = 0xE12 * 0x120
    print("[arithmetically derived bounds]")
    print(f"lt glyph bytes: 0xE12 * 0x120 = {glyph_bytes:#x}")
    print(f"lt allocation: 0x1FB * 0x800 = {0x1FB * 0x800:#x}")
    print(f"lt non-glyph tail: {0x1FB * 0x800 - glyph_bytes:#x} bytes")
    print("Static conclusion: zero-filling the tail is functionally supported by all located glyph consumers; byte-identical reconstruction still requires preserving it because no producer/checksum routine was found.")
    print()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("elf", type=Path)
    parser.add_argument("mapper", type=Path)
    args = parser.parse_args()
    image = ElfImage(args.elf)
    functions = FunctionMap(args.mapper)

    report_static_tables(image)
    report_call_census(image, functions)
    report_absolute_reference_census(image, functions)
    report_function_pointer_tables(image)
    report_derived_limits()
    for key in (
        "registry", "resource_allocations", "entry_reader", "package_loader",
        "gzip_block", "metadata_installer", "bundle_loader", "stream_setup",
        "bsf_setup", "bsf_step", "bk_loader", "addpt_loader", "pt_loader",
        "texture_copy", "palette_copy", "pr_lookup", "pr_direct_texture",
        "pr_compressed_atlas", "lt_renderer", "script_load", "script_reload",
        "script_layout", "script_voice_init", "script_global_flag",
        "script_global_secondary", "script_init", "script_lookup",
        "script_interpreter", "script_text_entry", "script_text_parser",
        "script_text_wrapper", "script_layer_image", "script_bk_request",
        "script_bk_commit", "itoc_search",
        "itoc_prefix_sum", "itoc_lookup",
    ):
        report_function(image, functions, key)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
