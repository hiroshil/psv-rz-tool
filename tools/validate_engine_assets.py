#!/usr/bin/env python3
"""Evidence-linked static/corpus validator for rz-tool 1.0.0.

This validator does not replace `cargo check`. It verifies the source-level
stable 1.0 contracts, the two-JSON SC corpus layout, executable-resident
sector/metadata tables, the patch sites used by `--eboot-in/--eboot-out`, and
(optionally) the exact Capstone evidence report.
"""
from __future__ import annotations

import argparse
import json
import pathlib
import re
import struct
import subprocess
import sys
import tempfile
import tomllib
import zipfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
SC_COUNT = 89
SECTOR_SIZE = 0x800
SC_SECTOR_TABLE = 0x8111344C
SC_METADATA_TABLE = 0x810F9B1C
SC_BUFFER_MOV = 0x8101B554
TABLES = (
    ("ADDPT_SECTORS", 0x811122DC, 1, 4, 2),
    ("PT_SECTORS", 0x81113394, 0x17, 4, 2),
    ("SC_SECTORS", SC_SECTOR_TABLE, SC_COUNT, 4, 2),
    ("BK_SECTORS", 0x810FA500, 0x146, 8, 4),
    ("BSF_SECTORS", 0x810FAF30, 0x126, 8, 4),
)
BUFFER_ENCODINGS = {
    0x00020000: bytes.fromhex("5ff40030"),
    0x00040000: bytes.fromhex("5ff48020"),
    0x00080000: bytes.fromhex("5ff40020"),
    0x00100000: bytes.fromhex("5ff48010"),
    0x00200000: bytes.fromhex("5ff40010"),
    0x00400000: bytes.fromhex("5ff48000"),
    0x00800000: bytes.fromhex("5ff40000"),
    0x01000000: bytes.fromhex("5ff08070"),
    0x02000000: bytes.fromhex("5ff00070"),
    0x04000000: bytes.fromhex("5ff08060"),
    0x08000000: bytes.fromhex("5ff00060"),
}


def read(relative: str) -> str:
    return (ROOT / relative).read_text(encoding="utf-8")


def validate_delimiters(source: str, path: pathlib.Path) -> None:
    pairs = {"{": "}", "(": ")", "[": "]"}
    for left, right in pairs.items():
        assert source.count(left) == source.count(right), (path, left, right)


def validate_source() -> dict:
    workspace = tomllib.loads(read("Cargo.toml"))
    assert workspace["workspace"]["package"]["version"] == "1.0.0"
    manifest = read("crates/rz-assets/src/manifest.rs")
    script = read("crates/rz-assets/src/codec/script.rs")
    pipeline = read("crates/rz-assets/src/pipeline.rs")
    eboot = read("crates/rz-assets/src/eboot.rs")
    charset = read("crates/rz-assets/src/codec/charset.rs")
    cli = read("crates/rz-tool/src/main.rs")
    package = read("crates/rz-assets/src/codec/engine_package.rs")
    gxt = read("crates/rz-assets/src/codec/gxt.rs")

    assert "PROJECT_SCHEMA_VERSION: u32 = 1" in manifest
    assert "const DOCUMENT_VERSION: u32 = 1" in script
    assert "const SOURCE_DOCUMENT_VERSION: u32 = 1" in script
    assert "const ROUTING_DOCUMENT_VERSION: u32 = 1" in script
    assert "speaker_source_glyphs" in script
    assert "encode_span_preserving_source" in script
    assert "unchanged_charset_alias_preserves_original_glyph_id" in script
    assert "multi_page_dialogue_round_trips_exactly" in script
    assert "scenario-routing.json" in script
    assert 'format!("{output_stem}.script.json")' in script
    assert 'format!("{output_stem}.script-meta.json")' in script
    decode_body = script[script.index("pub fn decode("):script.index("pub fn encode(")]
    assert "script-payload.txt" not in decode_body
    assert "script-text.json" not in decode_body
    assert "script-source.json" not in decode_body
    assert "std::mem::take(&mut source_document.secondary_records)" in decode_body
    assert "encode_with_allocation_and_charset" in script
    assert "inspect_build_with_charset" in script
    assert "--eboot-in" in cli and "--eboot-out" in cli and "--charset-map" in cli
    assert "SC_SECTOR_TABLE_VA: u32 = 0x8111_344c" in eboot
    assert "SC_METADATA_TABLE_VA: u32 = 0x810f_9b1c" in eboot
    assert "SC_BUFFER_MOV_VA: u32 = 0x8101_b554" in eboot
    assert "patch_sc_elf" in eboot
    assert "write_default_document" in charset and "load_document" in charset
    assert "charset.json" in pipeline
    assert 'format!("{:05}", file.id().expect("SC ID was validated"))' in pipeline
    assert "write_routing_document(stage, &assets)" in pipeline
    assert "sc.cpk does not contain every engine ID 0..88" in pipeline
    assert "const DOCUMENT_VERSION: u32 = 1" in package
    assert "preserved_decoded_fnv1a64" in package
    assert "source_table_fnv1a64" in package
    assert "source_parsed.decompressed.as_slice() != decoded.as_slice()" in package
    assert "verify_rebuilt_subpackage" in package
    assert "nearest_palette_index" not in package
    assert "encode_engine_texture_preserving_source" in package
    assert "build_chunk_table_incremental" in package
    assert "median_cut_palette" in package
    assert "texture_visual_error" in package
    assert "changed_bc_texture_is_reencoded_without_copy_only_gate" in package
    assert "paletted_encode_preserves_duplicate_source_index" in package
    assert "paletted_encode_adds_new_color_without_manual_json_edit" in package
    assert "paletted_encode_quantizes_when_color_count_exceeds_capacity" in package
    assert "incremental_table_reuses_only_unchanged_raw_blocks" in package
    assert "encode_texture_preserving_source" in gxt
    assert "block_visual_error" in gxt
    assert "representable resolution" in gxt
    assert "source_aware_bc_keeps_unchanged_blocks_byte_exact" in gxt
    assert not list((ROOT / "tools").glob("migrate_*"))
    analysis_files = sorted(path.name for path in ROOT.glob("*ANALYSIS*.md"))
    assert analysis_files == ["ENGINE_ANALYSIS.md"], analysis_files

    values = re.search(
        r"const GLYPH_CODEPOINTS: \[u32; GLYPH_COUNT\] = \[(.*?)\];",
        charset,
        re.S,
    ).group(1)
    codepoints = [int(value, 16) for value in re.findall(r"0x[0-9A-Fa-f]+", values)]
    assert len(codepoints) == 0xE12
    assert chr(codepoints[0x13B]) == "ス"
    assert chr(codepoints[0xA60]) == "任"

    for path in (ROOT / "crates").rglob("*.rs"):
        validate_delimiters(path.read_text(encoding="utf-8"), path)
    return {
        "workspace_version": "1.0.1",
        "schema": 1,
        "engine_package_document_version": 1,
        "script_meta_version": 1,
        "script_editable_version": 1,
        "scenario_routing_version": 1,
        "glyphs": len(codepoints),
    }


def count_secondary_labels(meta: dict) -> int:
    return sum(
        field.get("kind") == "label"
        for record in meta.get("secondary_records", [])
        for field in record.get("fields", [])
    )


def validate_corpus(root: pathlib.Path) -> dict:
    editable_paths = sorted(root.glob("[0-9][0-9][0-9][0-9][0-9].script.json"))
    meta_paths = sorted(root.glob("[0-9][0-9][0-9][0-9][0-9].script-meta.json"))
    assert len(editable_paths) == len(meta_paths) == SC_COUNT
    assert not list(root.glob("*.script-source.json"))
    assert not list(root.glob("*.script-text.json"))
    assert not list(root.glob("*.script-payload.txt"))
    assert not list(root.rglob("*skeleton.bin"))
    assert not [path for path in root.iterdir() if path.is_dir()]

    charset = json.loads((root / "charset.json").read_text(encoding="utf-8"))
    assert charset["document_version"] == 1
    assert charset["glyph_count"] == 0xE12
    assert len(charset["codepoints"]) == 0xE12
    codepoints = [chr(int(value[2:], 16)) for value in charset["codepoints"]]

    manifest = json.loads((root / "rz-project.json").read_text(encoding="utf-8"))
    assert manifest["schema_version"] == 1
    assets = manifest["source"]["assets"]
    assert len(assets) == SC_COUNT

    routing = json.loads((root / "scenario-routing.json").read_text(encoding="utf-8"))
    assert routing["document_version"] == 1
    assert routing["archive"] == "sc.cpk"
    assert routing["startup"]["entry_id"] == 0x56
    assert routing["startup"]["stream_id"] == 0
    assert "navigation evidence" in routing["transition_model"]
    assert len(routing["entries"]) == SC_COUNT
    assert [entry["entry_id"] for entry in routing["entries"]] == list(range(SC_COUNT))

    total_dialogues = 0
    total_pages = 0
    page_histogram = {1: 0, 2: 0, 3: 0}
    total_labels = 0
    raw_nodes = 0
    samples = []
    opening_line_found = False
    stream_counts = {}
    for entry_id in range(SC_COUNT):
        stem = f"{entry_id:05d}"
        editable = json.loads((root / f"{stem}.script.json").read_text(encoding="utf-8"))
        meta = json.loads((root / f"{stem}.script-meta.json").read_text(encoding="utf-8"))
        asset = next(asset for asset in assets if asset.get("id") == entry_id)
        assert editable["document_version"] == 1
        assert meta["document_version"] == 1
        assert editable["entry_id"] == meta["entry_id"] == entry_id
        assert editable["charset"] == charset["charset_id"]
        assert meta["editable"] == f"{stem}.script.json"
        assert meta["payload_capacity_bytes"] == meta["allocation_size"] - 0x2000 - 0x10
        assert len(bytes.fromhex(meta["opaque_footer_hex"])) == 0x10
        assert asset["document"] == f"{stem}.script-meta.json"
        assert editable.get("secondary_records", []) == []
        stream_counts[entry_id] = editable["stream_count"]
        dialogues = [node for node in editable["nodes"] if node["kind"] == "dialogue"]
        raws = [node for node in editable["nodes"] if node["kind"] == "raw"]
        total_dialogues += len(dialogues)
        raw_nodes += len(raws)
        total_labels += count_secondary_labels(meta)
        for expected, node in enumerate(dialogues):
            assert node["marker_index"] == expected
            assert isinstance(node["speaker"], str)
            assert isinstance(node["speaker_source_glyphs"], list)
            assert "text" not in node
            pages = node["pages"]
            assert 1 <= len(pages) <= 3
            page_histogram[len(pages)] += 1
            total_pages += len(pages)
            for page in pages:
                assert isinstance(page["text"], str)
                assert isinstance(page["source_glyphs"], list)
                assert "".join(codepoints[glyph] for glyph in page["source_glyphs"]) == page["text"]
                if "これ、本当にラムたちが" in page["text"]:
                    opening_line_found = entry_id == 86 and node["marker_index"] == 1
        if len(samples) < 3 and dialogues:
            samples.append({
                "entry": entry_id,
                "marker": dialogues[0]["marker_index"],
                "speaker": dialogues[0]["speaker"],
                "pages": [page["text"] for page in dialogues[0]["pages"]],
            })

    assert total_dialogues == 20686
    assert total_pages == 37125
    assert page_histogram == {1: 7927, 2: 9079, 3: 3680}
    assert total_labels == 119
    assert opening_line_found
    transitions = routing["transitions"]
    assert len(transitions) == 94
    for transition in transitions:
        assert transition["opcode"] == "FFEF"
        assert 0 <= transition["target_entry_id"] < SC_COUNT
        assert 0 <= transition["target_stream_id"] < stream_counts[transition["target_entry_id"]]
    return {
        "entries": SC_COUNT,
        "json_files_per_entry": 2,
        "dialogue_nodes": total_dialogues,
        "dialogue_pages": total_pages,
        "page_histogram": page_histogram,
        "raw_nodes": raw_nodes,
        "secondary_payload_labels": total_labels,
        "range_validated_ffef_route_candidates": len(transitions),
        "startup_entry": routing["startup"],
        "opening_line_recovered": opening_line_found,
        "external_skeletons": 0,
        "samples": samples,
    }


class Elf:
    def __init__(self, path: pathlib.Path):
        self.data = bytearray(path.read_bytes())
        assert self.data[:6] == b"\x7fELF\x01\x01"
        phoff = struct.unpack_from("<I", self.data, 0x1C)[0]
        entsize = struct.unpack_from("<H", self.data, 0x2A)[0]
        count = struct.unpack_from("<H", self.data, 0x2C)[0]
        self.segments = []
        for i in range(count):
            kind, offset, vaddr, _, filesz = struct.unpack_from(
                "<IIIII", self.data, phoff + i * entsize
            )
            if kind == 1 and filesz:
                self.segments.append((vaddr, offset, filesz))

    def offset(self, address: int, size: int) -> int:
        for vaddr, offset, filesz in self.segments:
            if vaddr <= address and address + size <= vaddr + filesz:
                return offset + address - vaddr
        raise AssertionError(f"ELF address outside PT_LOAD: {address:#x}+{size:#x}")

    def read(self, address: int, size: int) -> bytes:
        offset = self.offset(address, size)
        return bytes(self.data[offset : offset + size])


def source_array(name: str) -> list[int]:
    source = read("crates/rz-assets/src/engine_allocations.rs")
    body = re.search(rf"pub const {name}: \[[^\]]+\] = \[(.*?)\];", source, re.S)
    if body:
        return [int(value, 0) for value in re.findall(r"0x[0-9A-Fa-f]+|\b\d+\b", body.group(1))]
    scalar = re.search(rf"pub const {name}: [^=]+ = (0x[0-9A-Fa-f]+|\d+);", source)
    assert scalar, name
    return [int(scalar.group(1), 0)]


def validate_elf(path: pathlib.Path) -> dict:
    elf = Elf(path)
    report = {}
    for name, address, count, stride, field in TABLES:
        values = [
            int.from_bytes(elf.read(address + index * stride + field, 2), "little")
            for index in range(count)
        ]
        assert values == source_array(name), name
        report[name] = len(values)

    sector_raw = elf.read(SC_SECTOR_TABLE, SC_COUNT * 4)
    start = 0
    for index in range(SC_COUNT):
        found_start, count = struct.unpack_from("<HH", sector_raw, index * 4)
        assert found_start == start and count > 0
        start += count

    metadata_raw = elf.read(SC_METADATA_TABLE, SC_COUNT * 12)
    stream_base = primary_base = secondary_base = 0
    for index in range(SC_COUNT):
        sb, sc, pb, pc, xb, xc = struct.unpack_from("<6H", metadata_raw, index * 12)
        assert (sb, pb, xb) == (stream_base, primary_base, secondary_base)
        stream_base += sc
        primary_base += (pc + 7) // 8
        secondary_base += xc

    instruction = elf.read(SC_BUFFER_MOV, 4)
    assert instruction == BUFFER_ENCODINGS[0x20000]
    eboot_source = read("crates/rz-assets/src/eboot.rs").lower().replace("_", "")
    for value, encoding in BUFFER_ENCODINGS.items():
        assert f"0x{value:08x}" in eboot_source
        encoded_literal = ",".join(f"0x{byte:02x}" for byte in encoding)
        assert encoded_literal in eboot_source.replace(" ", "")
    report.update({
        "stock_sc_total_sectors": start,
        "stock_sc_buffer": 0x20000,
        "metadata_records": SC_COUNT,
        "patch_sites_mapped": 3,
    })
    return report


def validate_capstone(
    elf: pathlib.Path,
    secrect: pathlib.Path,
    capstone_path: pathlib.Path | None,
    capstone_wheel: pathlib.Path | None,
) -> bool:
    env = dict(**__import__("os").environ)
    with tempfile.TemporaryDirectory(prefix="rz-capstone-") as temporary:
        if capstone_wheel:
            with zipfile.ZipFile(capstone_wheel) as archive:
                archive.extractall(temporary)
            env["PYTHONPATH"] = temporary
        elif capstone_path:
            env["PYTHONPATH"] = str(capstone_path)
        process = subprocess.run(
            [sys.executable, str(ROOT / "tools/research/analyze_engine_assets.py"), str(elf), str(secrect)],
            check=True,
            capture_output=True,
            env=env,
        )
    assert process.stdout == (ROOT / "CAPSTONE_EVIDENCE.txt").read_bytes()
    return True


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--corpus", type=pathlib.Path)
    parser.add_argument("--elf", type=pathlib.Path)
    parser.add_argument("--secrect", type=pathlib.Path)
    parser.add_argument("--capstone-path", type=pathlib.Path, default=None)
    parser.add_argument("--capstone-wheel", type=pathlib.Path, default=None)
    parser.add_argument("--output", type=pathlib.Path)
    args = parser.parse_args()

    try:
        import capstone
    except ImportError:
        if args.capstone_path and args.capstone_wheel:
            parser.error("use either --capstone-path or --capstone-wheel, not both")

    report = {"source": validate_source()}
    if args.corpus:
        report["script_corpus"] = validate_corpus(args.corpus)
    if args.elf:
        report["elf"] = validate_elf(args.elf)
    if args.elf and args.secrect:
        report["capstone_report_exact"] = validate_capstone(
            args.elf, args.secrect, args.capstone_path, args.capstone_wheel
        )
    encoded = json.dumps(report, ensure_ascii=False, indent=2) + "\n"
    if args.output:
        args.output.write_text(encoded, encoding="utf-8")
    print(encoded, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
