#!/usr/bin/env python3
"""Patch stock RZ Vita EBOOT to a validated VWF runtime profile.

This patcher is intentionally semantic rather than byte-diff based:
- Thumb hooks are assembled with keystone-engine.
- The custom glyph range and runtime glyph limit are derived from font.tbl using a 0x40-glyph bucket plus one guard bucket instead of a manual CLI override.
- LT allocation/table values are not patched here; rz-tool owns LT allocation when it builds lt.bin.
- Width bytes are compiled from the same font.cnf/JSON used by rz-tool.
- The E37E helper uses one pixel-cursor model for both stock glyphs below 0x0E12 and custom VWF glyphs below glyph_range_end_exclusive; this fixes mixed stock/custom lines such as custom text followed by ラム. This behavior is promoted into v1-stable after Deploy A/B runtime validation.
- The speaker/nameplate raster path at FUN_8104DE8C has a separate fixed +0x18 advance. A candidate hook at 0x8104DF0C applies the same custom-glyph width table there while preserving stock 24px advance and the original glyph-0 12px override.
- FUN_8104DF94's stock 8-glyph copy cap is replaced by a bounded VWF pixel-budget filter. The CLI exposes --speaker-max-width/--speaker-max-width-px and rejects values above the stock 8x24=192px speaker envelope.
- SC allocation is deliberately not patched here; rz-tool owns SC allocation when it builds sc.cpk.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import struct
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable

BASE_VA = 0x81000000
CODE_LOAD_FILE_OFFSET = 0xE0
DATA_LOAD_VA = 0x81128000
DATA_LOAD_FILE_OFFSET = 0x127FE0

PATCHER_VERSION = "v1"
STOCK_SHA256 = "f5233a9ed15bedab8893800701a176da07986952fb0fb9ce92b33519327e1825"

# Engine constants proven in ENGINE_ANALYSIS.md.
STOCK_GLYPH_LIMIT = 0x0E12
WIDTH_TABLE_VA = 0x8112B070
GLYPH_BUCKET_SIZE = 0x40
GLYPH_GUARD_BUCKETS = 1

E37E_SITE_VA = 0x8104E37E
E37E_HELPER_VA = 0x8110B036
E37E_STOCK_CONTINUE_THUMB = 0x8104E383
E37E_RENDER_GLYPH_THUMB = 0x8101B3F7
E37E_UPDATE_CURSOR_THUMB = 0x8104E303
E37E_RENDER_STATE_VA = 0x8112AEB8
E37E_TEXTURE_STATE_PTR_VA = 0x811B98E4
E37E_GPU_FN_TABLE_VA = 0x8110C46C
E37E_GPU_BLIT_SIZE = 0x400
# Signature/trailer bytes observed in the verified helper block. Kept as data, not control logic.
E37E_HELPER_TRAILER = bytes.fromhex("12 0d 0d 08 07 0d 07 0d 0d 12 12 12 05 07 05 52 5a 56 57 46 33 33")

E240_SITE_VA = 0x8104E24C
E240_HELPER_VA = 0x8110B150
E240_RETURN_VA = 0x8104E250
E240_GUARD_ADD = 0x6A

SPEAKER_ADVANCE_SITE_VA = 0x8104DF0C
SPEAKER_ADVANCE_HELPER_VA = 0x8110B110
SPEAKER_ADVANCE_RETURN_THUMB = 0x8104DF11

# Speaker/nameplate string filtering. Stock FUN_8104DF94 copies at most 8
# source glyphs into a 12-halfword stack buffer. The surrounding speaker layout
# is centered in 8 stock cells (8 * 24 = 192 px). The VWF filter keeps that
# 192px visual envelope but admits more than 8 narrow glyphs by accumulating
# the same runtime advance widths used by FUN_8104DE8C.
SPEAKER_FILTER_SITE_VA = 0x8104E02E
SPEAKER_FILTER_HELPER_VA = 0x8110BF84
SPEAKER_FILTER_HELPER_CAPACITY = 0x74
SPEAKER_FILTER_RETURN_THUMB = 0x8104E049
SPEAKER_CENTER_COUNT_GUARD_SITE_VA = 0x8104DDE8
SPEAKER_STOCK_COLUMNS = 8
SPEAKER_CELL_WIDTH = 24
SPEAKER_ENGINE_MAX_WIDTH_PX = SPEAKER_STOCK_COLUMNS * SPEAKER_CELL_WIDTH  # 192px
SPEAKER_ENGINE_MAX_TOTAL_GLYPHS = 21  # sibling 0x6000-byte raster path FUN_8104E182
SPEAKER_TEMP_BUFFER_HALFWORDS = SPEAKER_ENGINE_MAX_TOTAL_GLYPHS + 1  # + terminator
SPEAKER_STACK_FRAME = 0x3C
SPEAKER_STACK_PARAM3_OFF = 0x2C
SPEAKER_STACK_PARAM2_OFF = 0x30
SPEAKER_STACK_CANARY_OFF = 0x34
SPEAKER_WRAPPER_GLYPH_WIDTH = 24
SPEAKER_MIN_WIDTH_PX = SPEAKER_WRAPPER_GLYPH_WIDTH * 2

ASCII_PADDING = " \t\r"

class PatcherError(RuntimeError):
    pass

@dataclass(frozen=True)
class FontProfile:
    glyph_range_start: int
    glyph_range_end_exclusive: int
    glyph_runtime_limit: int
    custom_glyph_count: int

    def to_json(self) -> dict[str, object]:
        return {
            "glyph_range_start": f"0x{self.glyph_range_start:04X}",
            "glyph_range_end_exclusive": f"0x{self.glyph_range_end_exclusive:04X}",
            "glyph_runtime_limit": f"0x{self.glyph_runtime_limit:04X}",
            "custom_glyph_count": self.custom_glyph_count,
            "runtime_limit_formula": "align_up(glyph_range_end_exclusive, 0x40) + 0x40",
            "lt_allocation_owner": "rz-tool build lt.bin --eboot-in/--eboot-out",
        }

@dataclass(frozen=True)
class ChangedRange:
    name: str
    va: int
    file_offset: int
    length: int
    sha256: str

    def to_json(self) -> dict[str, object]:
        return {
            "name": self.name,
            "va": hex(self.va),
            "file_offset": hex(self.file_offset),
            "length": self.length,
            "sha256": self.sha256,
        }


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def align_up(value: int, alignment: int) -> int:
    return (value + alignment - 1) & ~(alignment - 1)


def va_to_offset(va: int) -> int:
    if BASE_VA <= va < DATA_LOAD_VA:
        return CODE_LOAD_FILE_OFFSET + (va - BASE_VA)
    if va >= DATA_LOAD_VA:
        return DATA_LOAD_FILE_OFFSET + (va - DATA_LOAD_VA)
    raise PatcherError(f"VA {va:#x} is below mapped EBOOT range")


def read_at_va(data: bytes | bytearray, va: int, length: int) -> bytes:
    off = va_to_offset(va)
    end = off + length
    if off < 0 or end > len(data):
        raise PatcherError(f"read out of file: va={va:#x} len={length:#x} file_size={len(data):#x}")
    return bytes(data[off:end])


def write_at_va(data: bytearray, va: int, payload: bytes, name: str) -> ChangedRange:
    off = va_to_offset(va)
    end = off + len(payload)
    if off < 0 or end > len(data):
        raise PatcherError(f"{name} write out of file: va={va:#x} off={off:#x} len={len(payload):#x}")
    data[off:end] = payload
    return ChangedRange(name, va, off, len(payload), sha256_bytes(payload))


def write_guarded_va(data: bytearray, va: int, expected_old: bytes, payload: bytes, name: str) -> ChangedRange:
    actual = read_at_va(data, va, len(expected_old))
    if actual != expected_old:
        raise PatcherError(
            f"{name} old-byte guard failed at {va:#x}: expected {expected_old.hex()} got {actual.hex()}"
        )
    return write_at_va(data, va, payload, name)


def strip_ascii_padding(value: str) -> str:
    return value.strip(ASCII_PADDING)


def parse_tbl_value(right: str, path: Path, line_no: int) -> str:
    if right == "":
        raise PatcherError(f"{path}:{line_no}: empty character mapping")
    notation = strip_ascii_padding(right)
    if notation.upper().startswith("U+"):
        try:
            return chr(int(notation[2:], 16))
        except ValueError as exc:
            raise PatcherError(f"{path}:{line_no}: invalid codepoint {notation!r}") from exc
    chars = list(right)
    if len(chars) != 1:
        raise PatcherError(f"{path}:{line_no}: exact mapping must contain one Unicode scalar; use U+XXXX for whitespace")
    return chars[0]


def parse_font_tbl(path: Path) -> dict[int, str]:
    out: dict[int, str] = {}
    for line_no, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        left_trimmed = raw.lstrip(ASCII_PADDING)
        if not left_trimmed or left_trimmed.startswith("#"):
            continue
        if "=" not in raw:
            raise PatcherError(f"{path}:{line_no}: expected HEX=char or HEX=U+XXXX")
        left, right = raw.split("=", 1)
        try:
            glyph_id = int(strip_ascii_padding(left), 16)
        except ValueError as exc:
            raise PatcherError(f"{path}:{line_no}: invalid glyph id {left!r}") from exc
        if glyph_id in out:
            raise PatcherError(f"{path}:{line_no}: duplicate glyph id {glyph_id:04X}")
        out[glyph_id] = parse_tbl_value(right, path, line_no)
    if not out:
        raise PatcherError(f"{path}: no glyph mappings loaded")
    return out


def derive_runtime_limit_from_used_end(glyph_range_end: int) -> int:
    """Derive the engine LT/runtime capacity from the used glyph range.

    The VWF helper only treats glyph IDs below glyph_range_end as custom glyphs,
    so this is not user-visible glyph coverage.  The runtime limit is the
    loader/interpreter capacity patched into engine bounds.  A one-0x40-glyph
    guard bucket is retained because the exact used-end profile (0x0EF7 for the
    bundled font.tbl) has been runtime-rejected, while the bucket+guard profile
    (0x0F40) matches the verified internal patcher/runtime.
    """
    return align_up(glyph_range_end, GLYPH_BUCKET_SIZE) + GLYPH_BUCKET_SIZE * GLYPH_GUARD_BUCKETS


def derive_font_profile(font: dict[int, str]) -> FontProfile:
    custom_ids = [glyph_id for glyph_id in font if glyph_id >= STOCK_GLYPH_LIMIT]
    if not custom_ids:
        raise PatcherError(f"font.tbl has no expanded glyph IDs >= {STOCK_GLYPH_LIMIT:04X}")
    glyph_range_start = STOCK_GLYPH_LIMIT
    glyph_range_end = max(custom_ids) + 1
    runtime_limit = derive_runtime_limit_from_used_end(glyph_range_end)
    if runtime_limit > 0xFFFF:
        raise PatcherError(f"runtime glyph limit out of u16 range: {runtime_limit:#x}")
    return FontProfile(
        glyph_range_start=glyph_range_start,
        glyph_range_end_exclusive=glyph_range_end,
        glyph_runtime_limit=runtime_limit,
        custom_glyph_count=glyph_range_end - glyph_range_start,
    )


def parse_width_key(key: str) -> str:
    key = strip_ascii_padding(key)
    if key == "<space>":
        return " "
    if key.lower().startswith("u+"):
        return chr(int(key[2:], 16))
    if len(key) != 1:
        raise PatcherError(f"width key {key!r} is not one character; use U+XXXX")
    return key


def expand_group_spec(spec: str) -> list[str]:
    vi_lower = "àáảãạằắẳẵặầấẩẫậèéẻẽẹềếểễệìíỉĩịòóỏõọồốổỗộờớởỡợùúủũụừứửữựỳýỷỹỵăâêôơưđ"
    vi_upper = "ÀÁẢÃẠẰẮẲẴẶẦẤẨẪẬÈÉẺẼẸỀẾỂỄỆÌÍỈĨỊÒÓỎÕỌỒỐỔỖỘỜỚỞỠỢÙÚỦŨỤỪỨỬỮỰỲÝỶỸỴĂÂÊÔƠƯĐ"
    tokens = {
        "space": " ",
        "upper": "ABCDEFGHIJKLMNOPQRSTUVWXYZ",
        "lower": "abcdefghijklmnopqrstuvwxyz",
        "ascii-upper": "ABCDEFGHIJKLMNOPQRSTUVWXYZ",
        "ascii-lower": "abcdefghijklmnopqrstuvwxyz",
        "digits": "0123456789",
        "ascii-printable": "".join(chr(code) for code in range(0x20, 0x7F)),
        "vietnamese": vi_lower + vi_upper,
        "viet-lower": vi_lower,
        "viet-upper": vi_upper,
    }
    out: list[str] = []
    index = 0
    while index < len(spec):
        if spec[index] == "<":
            end = spec.find(">", index + 1)
            if end != -1:
                token = spec[index + 1:end]
                if token in tokens:
                    out.extend(tokens[token])
                    index = end + 1
                    continue
        out.append(spec[index])
        index += 1
    return out


def strip_font_config_inline_comment(value: str) -> str:
    # For non-group values, '#' starts a full-line or whitespace-separated comment.
    for index, ch in enumerate(value):
        if ch == "#" and (index == 0 or value[index - 1].isspace()):
            return value[:index]
    return value


def strip_font_config_group_comment(value: str) -> str:
    # In font.cnf [groups], '#' can be a real glyph, e.g. WIDE=#$%&@mw.
    # Only a whitespace-separated '#' starts a trailing comment.
    for index, ch in enumerate(value):
        if ch == "#" and index > 0 and value[index - 1].isspace():
            return value[:index].rstrip()
    return value.rstrip()


def iter_config_lines(path: Path) -> Iterable[tuple[int, str]]:
    for line_no, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        stripped = raw.strip()
        if not stripped or stripped.startswith("#"):
            continue
        yield line_no, stripped


def parse_width_table(path: Path) -> tuple[dict[str, int], int]:
    if path.suffix.lower() == ".json":
        data = json.loads(path.read_text(encoding="utf-8"))
        glyphs = data.get("glyphs")
        if not isinstance(glyphs, list):
            raise PatcherError(f"{path}: JSON width table must contain glyphs[]")
        return {item["char"]: int(item["advance"]) for item in glyphs}, int(data.get("default_advance", 24))

    section = ""
    buckets: dict[str, int] = {}
    groups: list[tuple[str, str]] = []
    advance_groups: list[tuple[int, str]] = []
    chars: dict[str, int] = {}
    default_bucket: str | None = None
    default_advance: int | None = None
    for line_no, line in iter_config_lines(path):
        if line.startswith("[") and line.endswith("]"):
            section = line[1:-1].strip().lower()
            continue
        if "=" not in line:
            raise PatcherError(f"{path}:{line_no}: expected key=value")
        key, raw_value = [part.strip() for part in line.split("=", 1)]
        value = raw_value.rstrip() if section == "groups" else strip_font_config_inline_comment(raw_value).strip()
        if section == "buckets":
            buckets[key] = int(value, 0)
        elif section == "groups":
            groups.append((key, strip_font_config_group_comment(raw_value)))
        elif section in {"advance_groups", "advance-groups", "width_groups", "width-groups"}:
            advance = int(key, 0)
            if not 0 <= advance <= 255:
                raise PatcherError(f"{path}:{line_no}: advance group key out of byte range: {advance}")
            advance_groups.append((advance, strip_font_config_group_comment(raw_value)))
        elif section in {"chars", "widths"}:
            chars[parse_width_key(key)] = int(value, 0)
        elif section == "default" and key == "bucket":
            default_bucket = value
        elif section == "default" and key == "advance":
            default_advance = int(value, 0)
        else:
            raise PatcherError(f"{path}:{line_no}: unsupported section/key [{section}] {key}")

    if default_advance is None:
        default_advance = buckets.get(default_bucket, 24) if default_bucket else 24
    if not 0 <= default_advance <= 255:
        raise PatcherError(f"{path}: default advance out of byte range: {default_advance}")

    widths: dict[str, int] = {}
    for bucket, spec in groups:
        if bucket not in buckets:
            raise PatcherError(f"{path}: group references unknown bucket {bucket!r}")
        advance = buckets[bucket]
        if not 0 <= advance <= 255:
            raise PatcherError(f"{path}: bucket {bucket!r} advance out of byte range: {advance}")
        for ch in expand_group_spec(spec):
            widths[ch] = advance
    for advance, spec in advance_groups:
        for ch in expand_group_spec(spec):
            widths[ch] = advance
    for ch, advance in chars.items():
        if not 0 <= advance <= 255:
            raise PatcherError(f"{path}: char {ch!r} advance out of byte range: {advance}")
        widths[ch] = advance
    return widths, default_advance


def compile_width_bytes(font: dict[int, str], profile: FontProfile, width_table: Path) -> tuple[bytes, dict[str, object]]:
    widths, default = parse_width_table(width_table)
    out = bytearray()
    missing_width_chars: set[str] = set()
    missing_glyph_ids: list[str] = []
    for glyph_id in range(profile.glyph_range_start, profile.glyph_range_end_exclusive):
        ch = font.get(glyph_id)
        if ch is None:
            out.append(default)
            missing_glyph_ids.append(f"{glyph_id:04X}")
            continue
        if ch not in widths:
            missing_width_chars.add(ch)
        out.append(widths.get(ch, default))
    summary = {
        "glyph_count": len(out),
        "default_advance": default,
        "missing_glyph_id_count": len(missing_glyph_ids),
        "missing_glyph_ids_sample": missing_glyph_ids[:16],
        "missing_width_char_count": len(missing_width_chars),
        "missing_width_chars_sample": sorted(missing_width_chars)[:16],
    }
    return bytes(out), summary


def assemble_thumb(address: int, source: str) -> bytes:
    try:
        from keystone import KS_ARCH_ARM, KS_MODE_THUMB, Ks
    except ModuleNotFoundError as exc:
        raise PatcherError("keystone-engine is required; install or set PYTHONPATH to the extracted wheel") from exc
    ks = Ks(KS_ARCH_ARM, KS_MODE_THUMB)
    encoded, _ = ks.asm(source, addr=address)
    return bytes(encoded)


def low16(value: int) -> int:
    return value & 0xFFFF


def high16(value: int) -> int:
    return (value >> 16) & 0xFFFF


def build_e37e_helper(profile: FontProfile) -> bytes:
    source = f"""
        push.w {{r4, r5, r6, r7, r8, lr}}
        uxth r4, r0
        movw ip, #{profile.glyph_range_end_exclusive:#x}
        cmp r4, ip
        bhs stock_fallback
        b render_with_pixel_cursor
    stock_fallback:
        movw ip, #{low16(E37E_STOCK_CONTINUE_THUMB):#x}
        movt ip, #{high16(E37E_STOCK_CONTINUE_THUMB):#x}
        bx ip
    render_with_pixel_cursor:
        sub sp, #8
        adds r5, r1, #0
        uxtb r6, r2
        uxtb r7, r3
        adds r0, r4, #0
        movw ip, #{low16(E37E_RENDER_GLYPH_THUMB):#x}
        movt ip, #{high16(E37E_RENDER_GLYPH_THUMB):#x}
        blx ip
        movw r8, #{low16(E37E_RENDER_STATE_VA):#x}
        movt r8, #{high16(E37E_RENDER_STATE_VA):#x}
        ldrh.w r0, [r8, #0x12]
        ldrb.w r1, [r8, #1]
        add.w r1, r1, r1, lsl #1
        lsls r1, r1, #0xd
        add.w r2, r0, r1
        movw r0, #{low16(E37E_TEXTURE_STATE_PTR_VA):#x}
        movt r0, #{high16(E37E_TEXTURE_STATE_PTR_VA):#x}
        ldr r1, [r0]
        movw r0, #{low16(E37E_GPU_FN_TABLE_VA):#x}
        movt r0, #{high16(E37E_GPU_FN_TABLE_VA):#x}
        adds r2, r5, r2
        ldr r5, [r0, #4]
        movs.w r0, #{E37E_GPU_BLIT_SIZE:#x}
        str r0, [sp]
        adds r0, r4, #0
        adds r3, r7, #0
        blx r5
        ldrh.w r0, [r8, #0x12]
        movw ip, #{profile.glyph_range_start:#x}
        cmp r4, ip
        blo stock_width
        sub.w r1, r4, ip
        movw r2, #{low16(WIDTH_TABLE_VA):#x}
        movt r2, #{high16(WIDTH_TABLE_VA):#x}
        ldrb r1, [r2, r1]
        b apply_advance
    stock_width:
        movs r1, #0x18
    apply_advance:
        add r0, r1
        ldrb.w r1, [r8]
        adds r2, r1, #1
        strh.w r0, [r8, #0x12]
        ldrb.w r0, [r8, #2]
        ldrb.w r1, [r8, #3]
        strb.w r2, [r8]
        movw ip, #{low16(E37E_UPDATE_CURSOR_THUMB):#x}
        movt ip, #{high16(E37E_UPDATE_CURSOR_THUMB):#x}
        blx ip
        ldrsh.w r0, [r8, #0xc]
        adds r0, r0, r6
        strh.w r0, [r8, #0xc]
        movs r0, #1
        add sp, #8
        pop.w {{r4, r5, r6, r7, r8, pc}}
    """
    return assemble_thumb(E37E_HELPER_VA, source) + E37E_HELPER_TRAILER

def build_speaker_advance_patch(profile: FontProfile) -> tuple[bytes, bytes]:
    """Patch the speaker/nameplate raster loop to advance custom glyphs by width_table.

    Static call-chain evidence:
      FUN_8104E404 case 0xFFF0 -> FUN_8104DF94 -> FUN_8104DE8C

    At 0x8104DF08 the stock loop has the current glyph in r2. The overwritten
    instruction at 0x8104DF0C computes r1 = r6 + 0x18. The helper preserves
    r2, returns the next X in r1, and intentionally clobbers only r0/ip; r0 is
    immediately reloaded by the original instruction at 0x8104DF10. The stock
    glyph-0 half-cell override at 0x8104DF16 remains untouched.
    """
    site = assemble_thumb(SPEAKER_ADVANCE_SITE_VA, f"b.w #{SPEAKER_ADVANCE_HELPER_VA:#x}")
    helper = assemble_thumb(
        SPEAKER_ADVANCE_HELPER_VA,
        f"""
        movw ip, #{profile.glyph_range_start:#x}
        cmp r2, ip
        blo stock_width
        movw ip, #{profile.glyph_range_end_exclusive:#x}
        cmp r2, ip
        bhs stock_width
        movw ip, #{profile.glyph_range_start:#x}
        sub.w r0, r2, ip
        movw r1, #{low16(WIDTH_TABLE_VA):#x}
        movt r1, #{high16(WIDTH_TABLE_VA):#x}
        ldrb r0, [r1, r0]
        add r1, r6, r0
        b return_to_stock
    stock_width:
        adds.w r1, r6, #0x18
    return_to_stock:
        movw ip, #{low16(SPEAKER_ADVANCE_RETURN_THUMB):#x}
        movt ip, #{high16(SPEAKER_ADVANCE_RETURN_THUMB):#x}
        bx ip
        """,
    )
    cave_capacity = E240_HELPER_VA - SPEAKER_ADVANCE_HELPER_VA
    if len(helper) > cave_capacity:
        raise PatcherError(
            f"speaker helper is too large for reserved cave: {len(helper):#x} > {cave_capacity:#x}"
        )
    return site, helper



def validate_speaker_max_width(value: int) -> int:
    if value < SPEAKER_MIN_WIDTH_PX or value > SPEAKER_ENGINE_MAX_WIDTH_PX:
        raise PatcherError(
            f"speaker max width must be {SPEAKER_MIN_WIDTH_PX}..{SPEAKER_ENGINE_MAX_WIDTH_PX} px; "
            f"got {value}. The upper bound is the stock 8-cell speaker envelope "
            f"({SPEAKER_STOCK_COLUMNS} * {SPEAKER_CELL_WIDTH}px)."
        )
    return value


def build_speaker_filter_patch(profile: FontProfile, max_width_px: int) -> tuple[bytes, bytes]:
    """Replace DF94's fixed 8-glyph copy cap with a bounded VWF pixel filter.

    Register contract at 0x8104E02E:
      r0  current glyph
      r1  wrapper flag (nonzero adds stock glyphs 0x39 + 0x3A)
      r4  speaker render-state pointer
      r7  output glyph count (already 1 when wrapper prefix was emitted)
      r8  stop-copy flag (also used by stock for the 0x29 delimiter)
      r10 accumulated pixel width (patched from the stock count limit)
      lr  output halfword pointer

    The helper preserves the stock 0x29 stop semantics, uses the same VWF width
    lookup as the speaker advance hook, and reserves 24px for the suffix when
    wrapper mode is active. It also retains an independent 21-visible-glyph cap
    matching sibling FUN_8104E182's 0x6000-byte raster path, so a pathological
    width table (including zero-width glyphs) cannot overflow the expanded
    22-halfword temporary buffer.

    Strings may exceed 8 glyphs only when the speaker alignment mode is the
    stock centered mode (2). This makes the DDD0 count>8 bypass below safe for
    the normal speaker path while preserving the old cap for other modes.
    """
    max_width_px = validate_speaker_max_width(max_width_px)
    site = assemble_thumb(SPEAKER_FILTER_SITE_VA, f"b.w #{SPEAKER_FILTER_HELPER_VA:#x}")
    helper = assemble_thumb(
        SPEAKER_FILTER_HELPER_VA,
        f"""
        cmp r0, #0x29
        beq stop_copy
        cmp.w r8, #0
        bne return_stock_loop

        cmp r7, #{SPEAKER_STOCK_COLUMNS}
        blt count_cap
        ldrh r2, [r4, #8]
        cmp r2, #2
        bne stop_copy

    count_cap:
        cbz r1, no_wrapper_count
        cmp r7, #{SPEAKER_ENGINE_MAX_TOTAL_GLYPHS - 1}
        bge stop_copy
        b width_lookup
    no_wrapper_count:
        cmp r7, #{SPEAKER_ENGINE_MAX_TOTAL_GLYPHS}
        bge stop_copy

    width_lookup:
        cmp r0, #0
        beq half_width
        movw ip, #{profile.glyph_range_start:#x}
        cmp r0, ip
        blo stock_width
        movw ip, #{profile.glyph_range_end_exclusive:#x}
        cmp r0, ip
        bhs stock_width
        movw ip, #{profile.glyph_range_start:#x}
        sub.w r2, r0, ip
        movw r3, #{low16(WIDTH_TABLE_VA):#x}
        movt r3, #{high16(WIDTH_TABLE_VA):#x}
        ldrb r2, [r3, r2]
        b have_width
    stock_width:
        movs r2, #{SPEAKER_CELL_WIDTH}
        b have_width
    half_width:
        movs r2, #12
    have_width:
        add.w r3, r10, r2
        cbz r1, compare_budget
        adds r3, #{SPEAKER_WRAPPER_GLYPH_WIDTH}
    compare_budget:
        cmp r3, #{max_width_px}
        bhi stop_copy
        strh r0, [lr], #2
        adds r7, #1
        add r10, r2
        b return_stock_loop
    stop_copy:
        movs.w r8, #1
    return_stock_loop:
        movw ip, #{low16(SPEAKER_FILTER_RETURN_THUMB):#x}
        movt ip, #{high16(SPEAKER_FILTER_RETURN_THUMB):#x}
        bx ip
        """,
    )
    if len(helper) > SPEAKER_FILTER_HELPER_CAPACITY:
        raise PatcherError(
            f"speaker filter helper is too large for reserved cave: "
            f"{len(helper):#x} > {SPEAKER_FILTER_HELPER_CAPACITY:#x}"
        )
    return site, helper


def apply_speaker_length_filter_patches(
    data: bytearray,
    changed: list[ChangedRange],
    profile: FontProfile,
    max_width_px: int,
) -> None:
    """Expand DF94's temporary buffer and install the VWF pixel-budget filter."""
    validate_speaker_max_width(max_width_px)

    # Expand the local speaker buffer from 12 to 22 halfwords, matching the
    # sibling 21-glyph renderer's capacity plus one 0xFFFF terminator slot.
    stack_rewrites = [
        (0x8104DF98, "sub sp, #0x24", f"sub sp, #{SPEAKER_STACK_FRAME:#x}", "speaker_stack_frame_alloc"),
        (0x8104DFA4, "str r3, [sp, #0x20]", f"str r3, [sp, #{SPEAKER_STACK_CANARY_OFF:#x}]", "speaker_stack_canary_store"),
        (0x8104DFB0, "movs r2, #0x16", f"movs r2, #{SPEAKER_TEMP_BUFFER_HALFWORDS * 2:#x}", "speaker_stack_buffer_init_size"),
        (0x8104DFDE, "strb.w r5, [sp, #0x1c]", f"strb.w r5, [sp, #{SPEAKER_STACK_PARAM2_OFF:#x}]", "speaker_stack_param2_store"),
        (0x8104E00A, "str r4, [sp, #0x18]", f"str r4, [sp, #{SPEAKER_STACK_PARAM3_OFF:#x}]", "speaker_stack_param3_store"),
        (0x8104E06C, "ldr r2, [sp, #0x18]", f"ldr r2, [sp, #{SPEAKER_STACK_PARAM3_OFF:#x}]", "speaker_stack_param3_load"),
        (0x8104E074, "ldrb.w r1, [sp, #0x1c]", f"ldrb.w r1, [sp, #{SPEAKER_STACK_PARAM2_OFF:#x}]", "speaker_stack_param2_load"),
        (0x8104E08A, "ldr r2, [sp, #0x20]", f"ldr r2, [sp, #{SPEAKER_STACK_CANARY_OFF:#x}]", "speaker_stack_canary_load"),
        (0x8104E09A, "add sp, #0x24", f"add sp, #{SPEAKER_STACK_FRAME:#x}", "speaker_stack_frame_free"),
    ]
    for va, old_src, new_src, name in stack_rewrites:
        old = assemble_thumb(va, old_src)
        new = assemble_thumb(va, new_src)
        if len(old) != len(new):
            raise PatcherError(
                f"{name} instruction size changed at {va:#x}: {len(old)} -> {len(new)}"
            )
        changed.append(write_guarded_va(data, va, old, new, name))

    # r10 used to hold the glyph-count limits 8/9. Re-purpose it as the pixel
    # width accumulated so far; wrapper prefix 0x39 contributes one stock cell.
    for va, old_src, new_src, name in [
        (0x8104E010, "mov.w r10, #8", "mov.w r10, #0", "speaker_pixel_accumulator_init"),
        (0x8104E01E, "movs.w r10, #9", f"movs.w r10, #{SPEAKER_WRAPPER_GLYPH_WIDTH}", "speaker_pixel_accumulator_wrapper_init"),
    ]:
        old = assemble_thumb(va, old_src)
        new = assemble_thumb(va, new_src)
        if len(old) != len(new):
            raise PatcherError(
                f"{name} instruction size changed at {va:#x}: {len(old)} -> {len(new)}"
            )
        changed.append(write_guarded_va(data, va, old, new, name))

    filter_site, filter_helper = build_speaker_filter_patch(profile, max_width_px)
    # Stock bytes: cmp r7,r10 ; bge 0x8104E048. Keep this literal because
    # some non-Keystone assemblers conservatively widen the forward bge.
    old_filter_site = bytes.fromhex("57 45 0a da")
    if len(old_filter_site) != len(filter_site):
        raise PatcherError(
            f"speaker filter hook size mismatch: {len(old_filter_site)} -> {len(filter_site)}"
        )
    changed.append(
        write_guarded_va(
            data,
            SPEAKER_FILTER_SITE_VA,
            old_filter_site,
            filter_site,
            "speaker_vwf_width_filter_branch",
        )
    )
    changed.append(
        write_guarded_va(
            data,
            SPEAKER_FILTER_HELPER_VA,
            bytes(len(filter_helper)),
            filter_helper,
            "speaker_vwf_width_filter_helper",
        )
    )

    # DDD0 normally returns zero when glyph_count > max_columns. In centered
    # mode that blocks the existing DE8C VWF correction. Removing only this
    # early branch lets the two stock terms combine algebraically to:
    #   ((8-count)*24 + (count*24-pixel_width)) / 2
    # = (192-pixel_width)/2
    # The filter above permits count>8 only for alignment mode 2.
    # Stock short branch bgt 0x8104DDEC -> 00 DC. Replace with Thumb NOP.
    old_guard = bytes.fromhex("00 dc")
    new_guard = bytes.fromhex("00 bf")
    if len(old_guard) != len(new_guard):
        raise PatcherError(
            f"speaker center count guard size mismatch: {len(old_guard)} -> {len(new_guard)}"
        )
    changed.append(
        write_guarded_va(
            data,
            SPEAKER_CENTER_COUNT_GUARD_SITE_VA,
            old_guard,
            new_guard,
            "speaker_center_allow_vwf_count_over_8",
        )
    )

def build_e240_guard_patch() -> tuple[bytes, bytes]:
    site = assemble_thumb(E240_SITE_VA, f"b.w #{E240_HELPER_VA:#x}")
    helper = assemble_thumb(
        E240_HELPER_VA,
        f"uxtb.w r8, r2; add.w r8, r8, #{E240_GUARD_ADD}; b.w #{E240_RETURN_VA:#x}",
    )
    return site, helper


def default_path(name: str) -> Path:
    return Path(__file__).resolve().parent / "examples" / name


def build_argument_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=f"Patch stock RZ EBOOT to a font.tbl-derived Keystone-assembled VWF runtime profile ({PATCHER_VERSION}).")
    parser.add_argument("--eboot-in", required=True, type=Path, help="stock input EBOOT ELF")
    parser.add_argument("--eboot-out", required=True, type=Path, help="output patched EBOOT ELF")
    parser.add_argument("--font-tbl", type=Path, default=default_path("font.tbl"), help="glyph-id to character map; custom range/end is derived from this file")
    parser.add_argument("--width-table", type=Path, default=default_path("font.cnf"), help="same font.cnf/JSON width config used by rz-tool --wrap-width-table")
    parser.add_argument("--summary-out", type=Path, default=None, help="optional JSON run summary path")
    parser.add_argument(
        "--speaker-max-width",
        "--speaker-max-width-px",
        dest="speaker_max_width",
        type=int,
        default=SPEAKER_ENGINE_MAX_WIDTH_PX,
        metavar="PX",
        help=(
            f"speaker/nameplate VWF pixel budget ({SPEAKER_MIN_WIDTH_PX}.."
            f"{SPEAKER_ENGINE_MAX_WIDTH_PX}; default: {SPEAKER_ENGINE_MAX_WIDTH_PX}). "
            "This replaces the stock 8-glyph copy cap while preserving the stock 8x24px envelope."
        ),
    )
    return parser


def run(argv: list[str] | None = None) -> int:
    ns = build_argument_parser().parse_args(argv)
    speaker_max_width = validate_speaker_max_width(ns.speaker_max_width)
    input_data = ns.eboot_in.read_bytes()
    input_sha = sha256_bytes(input_data)
    if input_sha != STOCK_SHA256:
        raise PatcherError(f"input is not the expected stock EBOOT: got {input_sha}, expected {STOCK_SHA256}")
    data = bytearray(input_data)

    font = parse_font_tbl(ns.font_tbl)
    profile = derive_font_profile(font)
    width_bytes, width_summary = compile_width_bytes(font, profile, ns.width_table)
    changed: list[ChangedRange] = []

    # Expanded glyph acceptance checks discovered before the e37e runtime hook.
    changed.append(write_guarded_va(data, 0x8100140E, assemble_thumb(0x8100140E, "movw r0, #0xe12"), assemble_thumb(0x8100140E, f"movw r0, #{profile.glyph_runtime_limit:#x}"), "vm_glyph_limit_main"))
    changed.append(write_guarded_va(data, 0x8102D37C, assemble_thumb(0x8102D37C, "movw r1, #0xe12"), assemble_thumb(0x8102D37C, f"movw r1, #{profile.glyph_runtime_limit:#x}"), "glyph_limit_aux"))
    changed.append(write_guarded_va(data, 0x8104DC38, assemble_thumb(0x8104DC38, "movw r0, #0xe12"), assemble_thumb(0x8104DC38, f"movw r0, #{profile.glyph_runtime_limit:#x}"), "string_raster_glyph_limit"))

    # Main VWF renderer hook and helper assembled with Keystone.
    e37e_branch = assemble_thumb(E37E_SITE_VA, f"b.w #{E37E_HELPER_VA:#x}")
    changed.append(write_guarded_va(data, E37E_SITE_VA, bytes.fromhex("2d e9 f0 41"), e37e_branch, "e37e_branch_to_helper"))
    changed.append(write_at_va(data, E37E_HELPER_VA, build_e37e_helper(profile), "e37e_vwf_helper_and_signature"))

    # Production width table.
    changed.append(write_at_va(data, WIDTH_TABLE_VA, width_bytes, "width_table"))

    # Speaker/nameplate raster path: replace the fixed +0x18 custom-glyph advance
    # in FUN_8104DE8C. This is a static candidate pending runtime speaker validation.
    speaker_site, speaker_helper = build_speaker_advance_patch(profile)
    old_speaker_advance = assemble_thumb(SPEAKER_ADVANCE_SITE_VA, "adds.w r1, r6, #0x18")
    changed.append(write_guarded_va(data, SPEAKER_ADVANCE_SITE_VA, old_speaker_advance, speaker_site, "speaker_vwf_advance_branch"))
    changed.append(write_guarded_va(data, SPEAKER_ADVANCE_HELPER_VA, bytes(len(speaker_helper)), speaker_helper, "speaker_vwf_advance_helper"))

    # Replace DF94's fixed 8-glyph speaker copy limit with a pixel budget while
    # preserving the stock 192px nameplate envelope and bounded scratch storage.
    apply_speaker_length_filter_patches(data, changed, profile, speaker_max_width)

    # High fixed-column guard retained from the validated dialogue VWF profile.
    e240_site, e240_helper = build_e240_guard_patch()
    changed.append(write_guarded_va(data, E240_SITE_VA, bytes.fromhex("5f fa 82 f8"), e240_site, "e240_branch_to_helper"))
    changed.append(write_at_va(data, E240_HELPER_VA, e240_helper, "e240_helper"))

    ns.eboot_out.parent.mkdir(parents=True, exist_ok=True)
    ns.eboot_out.write_bytes(data)
    output_sha = sha256_bytes(bytes(data))

    summary = {
        "patcher_version": PATCHER_VERSION,
        "input_eboot": str(ns.eboot_in),
        "input_sha256": input_sha,
        "output_eboot": str(ns.eboot_out),
        "output_sha256": output_sha,
        "font_tbl": str(ns.font_tbl),
        "width_table": str(ns.width_table),
        "profile": profile.to_json(),
        "width_table_sha256": sha256_bytes(width_bytes),
        "width_summary": width_summary,
        "speaker_limit": {
            "mode": "VWF pixel budget",
            "max_width_px": speaker_max_width,
            "hard_max_width_px": SPEAKER_ENGINE_MAX_WIDTH_PX,
            "stock_logical_columns": SPEAKER_STOCK_COLUMNS,
            "stock_cell_width_px": SPEAKER_CELL_WIDTH,
            "hard_total_glyph_cap": SPEAKER_ENGINE_MAX_TOTAL_GLYPHS,
            "temporary_buffer_halfwords": SPEAKER_TEMP_BUFFER_HALFWORDS,
            "wrapper_reserve_px": SPEAKER_WRAPPER_GLYPH_WIDTH if speaker_max_width >= SPEAKER_MIN_WIDTH_PX else 0,
        },
        "runtime_validation": {
            "deploy_A_cursor_single": "PASS: mixed custom VWF glyphs followed by stock ラム no longer leave a blank position or push ラム to row end",
            "deploy_B_continuation": "PASS: long continuation/wrap/fold-back test showed no visible runtime errors",
            "validated_fix_scope": "E37E mixed stock/custom pixel-cursor model; SC allocation remains rz-tool-owned",
            "speaker_vwf_candidate": "STATIC ONLY: speaker advance uses width_table; DF94 fixed 8-glyph cap is replaced by a bounded pixel budget <=192px; runtime speaker validation pending"
        },
        "sc_allocation_policy": "not patched by VWF patcher; rz-tool owns SC allocation during sc.cpk build",
        "lt_allocation_policy": "not patched by VWF patcher; rz-tool owns LT allocation during lt.bin build",
        "changed_ranges": [entry.to_json() for entry in changed],
    }
    summary_text = json.dumps(summary, indent=2, ensure_ascii=False)
    if ns.summary_out is not None:
        ns.summary_out.parent.mkdir(parents=True, exist_ok=True)
        ns.summary_out.write_text(summary_text + "\n", encoding="utf-8")
    sys.stdout.write(summary_text + "\n")
    return 0


def main() -> int:
    try:
        return run()
    except PatcherError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2

if __name__ == "__main__":
    raise SystemExit(main())
