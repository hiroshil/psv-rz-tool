#!/usr/bin/env python3
"""Evidence-linked static/corpus validator for rz-tool 1.0.1.

This validator does not replace `cargo check`. It verifies the source-level
stable contracts, the compact SC dialogue project layout, fail-closed text
coverage invariants, executable-resident sector/metadata tables, patch sites,
and optionally the exact Capstone evidence report.
"""
from __future__ import annotations

import argparse
import gzip
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
    assert workspace["workspace"]["package"]["version"] == "1.0.1"
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
    assert "const ROUTING_DOCUMENT_VERSION: u32 = 2" in script
    assert "const DIALOGUE_DOCUMENT_VERSION: u32 = 1" in script
    assert "const STATE_BUNDLE_VERSION: u32 = 2" in script
    assert 'const STATE_BUNDLE_PATH: &str = ".rz-internal/sc-state.json.gz"' in script
    assert "ScriptMachineState" in script
    assert "ScriptMachineDocument" in script
    assert "ScriptMachineNode" in script
    assert "ScriptGlyphAlias" in script
    assert "compact_script_state" in script
    assert "hydrate_script_state" in script
    assert "compact_source_document" in script
    assert "hydrate_machine_source" in script
    assert "encode_text_with_aliases" in script
    assert "ScriptTextGrammar" in script
    assert "SecondaryString" in script
    assert "InlineFf42" in script and "InlineFf8c" in script
    assert "encode_span_preserving_source" in script
    assert "unchanged_charset_alias_preserves_original_glyph_id" in script
    assert "multi_page_dialogue_round_trips_exactly" in script
    assert "validate_text_coverage" in script
    assert "validate_source_roundtrip" in script
    assert "refusing extraction to avoid hidden text loss" in script
    assert "scenario-dialogue.json" in script
    assert "compact_state_contains_no_duplicate_text_and_hydrates_exactly" in script
    assert ".rz-internal/sc-state.json.gz" in script
    assert "GzEncoder" in script and "GzDecoder" in script
    assert "debug/scenario-ir" not in script  # path is composed from components
    assert 'join("debug").join("scenario-ir")' in script
    assert "load_dialogue_document" in script
    assert "load_state_bundle" in script
    assert "apply_dialogue_entry" in script
    assert "source: compact_source_document(source, charset::default_map())?" in script
    assert "validate_source_text_coverage" in script
    assert "leaves a secondary-target string in raw IR" in script
    assert "leaves a proven FF42/FF8C inline string in raw IR" in script
    assert "unanchored_low_operands_remain_raw_and_round_trip" in script
    assert "secondary_machine_suffix_low_operands_are_not_reclassified_as_text" in script
    assert "hides an FFFE dialogue-page terminator in machine suffix words" in script
    assert "edited_non_dialogue_text_reencodes_without_source_glyphs" in script
    assert "scenario-routing.json" in script
    assert "scenario-dialogue.json" in script
    assert "build_scenario_navigation" in script
    assert "apply_presentation_order" in script
    assert '"{:05}-id{:05}"' in script
    assert 'format!(".rz-internal/sc/{editable_file_name}")' in script
    assert 'format!(".rz-internal/sc/id{entry_id:05}.script-state.json")' in script
    assert "temporary_entry_directory" in script
    assert "fs::remove_dir_all(temporary_entry_directory)" in script
    decode_body = script[script.index("pub fn decode("):script.index("pub fn encode(")]
    assert "script-payload.txt" not in decode_body
    assert "script-text.json" not in decode_body
    assert "script-source.json" not in decode_body
    assert "std::mem::take(&mut source_document.secondary_records)" in decode_body
    assert "encode_state_with_allocation_and_charset_and_dialogue" in script
    assert "inspect_state_build_with_charset_and_dialogue" in script
    assert "--eboot-in" in cli and "--eboot-out" in cli and "--charset-map" in cli and "--debug-script-ir" in cli
    assert "SC_SECTOR_TABLE_VA: u32 = 0x8111_344c" in eboot
    assert "SC_METADATA_TABLE_VA: u32 = 0x810f_9b1c" in eboot
    assert "SC_BUFFER_MOV_VA: u32 = 0x8101_b554" in eboot
    assert "patch_sc_elf" in eboot
    assert "write_default_document" in charset and "load_document" in charset
    assert "charset.json" in pipeline
    assert 'format!("{:05}", file.id().expect("SC ID was validated"))' in pipeline
    assert "processing_order.sort_by_key" in pipeline
    assert "options.debug_script_ir" in pipeline
    assert "apply_presentation_order" in pipeline
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
    assert "#[cfg(test)]\nfn build_chunk_table(" in package
    assert "fn encode_color_block(pixels:" not in gxt
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
        "script_internal_ir_version": 1,
        "scenario_dialogue_version": 1,
        "scenario_routing_version": 2,
        "scenario_state_bundle_version": 2,
        "glyphs": len(codepoints),
    }


def count_secondary_labels(meta: dict) -> int:
    return sum(
        field.get("kind") == "label"
        for record in meta.get("secondary_records", [])
        for field in record.get("fields", [])
    )



def derive_navigation_order(
    entry_ids: list[int],
    transitions: list[dict],
    startup_entry_id: int,
) -> list[dict]:
    from collections import deque

    adjacency: dict[int, list[int]] = {}
    global_indegree = {entry_id: 0 for entry_id in entry_ids}
    for transition in sorted(
        transitions,
        key=lambda item: (
            item["source_entry_id"],
            item["source_node_index"],
            item["source_word_index"],
        ),
    ):
        source = transition["source_entry_id"]
        target = transition["target_entry_id"]
        targets = adjacency.setdefault(source, [])
        if target not in targets:
            targets.append(target)
            global_indegree[target] += 1

    entry_set = set(entry_ids)
    visited: set[int] = set()
    output: list[dict] = []

    def append_component(root: int, reachable: bool) -> None:
        if root not in entry_set or root in visited:
            return
        discovery = {root: 0}
        depth = {root: 0}
        queue = deque([root])
        while queue:
            source = queue.popleft()
            for target in adjacency.get(source, []):
                if target in entry_set and target not in visited and target not in discovery:
                    discovery[target] = len(discovery)
                    depth[target] = depth[source] + 1
                    queue.append(target)

        component = set(discovery)
        indegree = {entry_id: 0 for entry_id in component}
        for source in component:
            for target in adjacency.get(source, []):
                if target in component:
                    indegree[target] += 1
        emitted: set[int] = set()
        while len(emitted) < len(component):
            candidates = [
                entry_id
                for entry_id in component
                if entry_id not in emitted and indegree[entry_id] == 0
            ]
            if not candidates:
                candidates = [entry_id for entry_id in component if entry_id not in emitted]
            entry_id = min(candidates, key=lambda value: (discovery[value], value))
            emitted.add(entry_id)
            visited.add(entry_id)
            output.append({
                "navigation_order": len(output),
                "component_root_entry_id": root,
                "route_depth": depth.get(entry_id, 0),
                "reachable_from_startup": reachable,
                "entry_id": entry_id,
            })
            for target in adjacency.get(entry_id, []):
                if target in indegree:
                    indegree[target] = max(0, indegree[target] - 1)

    append_component(startup_entry_id, True)
    for entry_id in sorted(entry_ids):
        if entry_id not in visited and global_indegree[entry_id] == 0:
            append_component(entry_id, False)
    for entry_id in sorted(entry_ids):
        append_component(entry_id, False)
    return output

def validate_corpus(root: pathlib.Path) -> dict:
    assert not list(root.glob("*.script.json"))
    assert not list(root.glob("*.script-meta.json"))
    assert not list(root.glob("*-id*.json"))
    assert not (root / "debug").exists()
    assert not list(root.rglob("*skeleton.bin"))

    visible = {path.name for path in root.iterdir() if not path.name.startswith(".")}
    assert visible == {
        "charset.json",
        "rz-project.json",
        "scenario-dialogue.json",
        "scenario-routing.json",
    }, visible

    charset = json.loads((root / "charset.json").read_text(encoding="utf-8"))
    assert charset["document_version"] == 1
    assert charset["glyph_count"] == 0xE12
    codepoints = [chr(int(value[2:], 16)) for value in charset["codepoints"]]
    reverse = {}
    for index, character in enumerate(codepoints):
        reverse.setdefault(character, index)
    reverse["ー"] = 0x00D0

    manifest = json.loads((root / "rz-project.json").read_text(encoding="utf-8"))
    assert manifest["schema_version"] == 1
    assets = manifest["source"]["assets"]
    assert len(assets) == SC_COUNT
    assert sorted(asset["id"] for asset in assets) == list(range(SC_COUNT))
    assert sorted(asset["order"] for asset in assets) == list(range(SC_COUNT))
    assert all(asset["document"] == ".rz-internal/sc-state.json.gz" for asset in assets)

    dialogue = json.loads((root / "scenario-dialogue.json").read_text(encoding="utf-8"))
    routing = json.loads((root / "scenario-routing.json").read_text(encoding="utf-8"))
    assert dialogue.keys() == {"document_version", "archive", "entries"}
    assert dialogue["document_version"] == 1 and dialogue["archive"] == "sc.cpk"
    assert routing["document_version"] == 2 and routing["archive"] == "sc.cpk"
    assert routing["dialogue_document"] == "scenario-dialogue.json"
    assert routing["startup"] == {
        "entry_id": 86,
        "stream_id": 0,
        "evidence": routing["startup"]["evidence"],
    }
    assert len(dialogue["entries"]) == len(routing["entries"]) == SC_COUNT
    assert [entry["entry_id"] for entry in dialogue["entries"]] == [
        entry["entry_id"] for entry in routing["entries"]
    ]
    assert all("editable" not in entry for entry in routing["entries"])
    for entry in dialogue["entries"]:
        assert entry.keys() == {"entry_id", "dialogues", "texts"}
        for item in entry["dialogues"]:
            assert item.keys() == {"marker_index", "speaker", "pages"}
            assert item["pages"]
        for item in entry["texts"]:
            assert item.keys() == {"text_index", "grammar", "text"}
            assert item["grammar"] in {"secondary-string", "inline-ff42", "inline-ff8c"}

    bundle_path = root / ".rz-internal" / "sc-state.json.gz"
    assert bundle_path.is_file()
    assert not (root / ".rz-internal" / "sc").exists()
    with gzip.open(bundle_path, "rt", encoding="utf-8") as handle:
        bundle = json.load(handle)
    assert bundle["document_version"] == 2 and bundle["archive"] == "sc.cpk"
    assert len(bundle["entries"]) == SC_COUNT
    state_by_id = {entry["entry_id"]: entry for entry in bundle["entries"]}
    assert sorted(state_by_id) == list(range(SC_COUNT))

    def glyph_end(words: list[int], start: int) -> int | None:
        cursor = start
        while cursor < len(words) and words[cursor] < 0xE12:
            cursor += 1
        if cursor > start and cursor < len(words) and words[cursor] == 0xFFFF:
            return cursor
        return None

    def inline_text(words: list[int], index: int) -> bool:
        if words[index] not in (0xFF42, 0xFF8C):
            return False
        if (
            words[index] == 0xFF8C
            and index + 2 < len(words)
            and words[index + 1 : index + 3] == [0, 0xFFFF]
        ):
            return False
        return glyph_end(words, index + 1) is not None

    def validate_aliases(text: str, aliases: list[dict]) -> None:
        seen = set()
        for alias in aliases:
            assert alias.keys() == {"character_index", "glyph_id"}
            index = alias["character_index"]
            glyph_id = alias["glyph_id"]
            assert index not in seen
            seen.add(index)
            assert 0 <= index < len(text)
            assert 0 <= glyph_id < len(codepoints)
            assert codepoints[glyph_id] == text[index]
            assert reverse[text[index]] != glyph_id

    by_dialogue_id = {entry["entry_id"]: entry for entry in dialogue["entries"]}
    assert len(by_dialogue_id) == SC_COUNT
    total_dialogues = total_pages = total_texts = total_aliases = 0
    grammar_counts = {"secondary-string": 0, "inline-ff42": 0, "inline-ff8c": 0}
    residual_fffe = residual_secondary = residual_inline = residual_long = 0
    short_raw_runs = []
    opening_line_found = choice_found = ff42_found = ff8c_found = False
    secondary_machine_suffix_guard = False
    forbidden_state_keys = {"text", "speaker", "pages", "source_glyphs", "speaker_source_glyphs", "charset"}

    for entry_id, packed in state_by_id.items():
        state = packed["state"]
        machine = packed["source"]
        assert state["entry_id"] == machine["entry_id"] == entry_id
        assert state.keys() == {
            "document_version",
            "entry_id",
            "source_size",
            "allocation_size",
            "payload_capacity_bytes",
            "opaque_footer_hex",
            "voice_header",
            "trailing_byte",
            "secondary_records",
            "engine_metadata",
        }
        assert machine.keys() == {
            "document_version",
            "entry_id",
            "stream_count",
            "nodes",
            "relocation_model",
        }
        assert machine["document_version"] == 1
        assert not (forbidden_state_keys & machine.keys())

        public_entry = by_dialogue_id[entry_id]
        public_dialogues = {item["marker_index"]: item for item in public_entry["dialogues"]}
        public_texts = {item["text_index"]: item for item in public_entry["texts"]}
        seen_dialogues = set()
        seen_texts = set()
        expected_text_index = 0

        for node in machine["nodes"]:
            assert not (forbidden_state_keys & node.keys())
            if node["kind"] == "raw":
                words = node["words"]
                residual_fffe += words.count(0xFFFE)
                if any(label.startswith("secondary_") for label in node.get("labels", [])):
                    residual_secondary += glyph_end(words, 0) is not None
                residual_inline += any(inline_text(words, i) for i in range(len(words)))
                cursor = 0
                while cursor < len(words):
                    if words[cursor] >= 0xE12:
                        cursor += 1
                        continue
                    start = cursor
                    while cursor < len(words) and words[cursor] < 0xE12:
                        cursor += 1
                    if cursor < len(words) and words[cursor] == 0xFFFF:
                        length = cursor - start
                        if length >= 3:
                            residual_long += 1
                        elif length == 2:
                            short_raw_runs.append(
                                (entry_id, words[start:cursor], words[max(0, start - 3):start])
                            )
                continue

            if node["kind"] == "text":
                assert node["text_index"] == expected_text_index
                expected_text_index += 1
                item = public_texts[node["text_index"]]
                assert item["grammar"] == node["grammar"]
                assert "prefix_words" in node and "suffix_words" in node
                aliases = node.get("glyph_aliases", [])
                validate_aliases(item["text"], aliases)
                total_aliases += len(aliases)
                seen_texts.add(node["text_index"])
                if (
                    entry_id == 17
                    and node["text_index"] == 6
                    and node["grammar"] == "secondary-string"
                    and item["text"] == "キスといえば口"
                    and node["suffix_words"] == [0x0004, 0x0019, 0x000E, 0xFFFF]
                ):
                    secondary_machine_suffix_guard = True
                continue

            assert node["kind"] == "dialogue"
            item = public_dialogues[node["marker_index"]]
            validate_aliases(item["speaker"], node.get("speaker_glyph_aliases", []))
            total_aliases += len(node.get("speaker_glyph_aliases", []))
            page_aliases = node.get("page_glyph_aliases", [])
            assert len(page_aliases) <= len(item["pages"])
            for index, aliases in enumerate(page_aliases):
                validate_aliases(item["pages"][index], aliases)
                total_aliases += len(aliases)
            seen_dialogues.add(node["marker_index"])

        assert seen_dialogues == set(public_dialogues)
        assert seen_texts == set(public_texts)
        for item in public_entry["dialogues"]:
            if "これ、本当にラムたちが" in item["pages"]:
                opening_line_found = entry_id == 86 and item["marker_index"] == 1
            total_pages += len(item["pages"])
        total_dialogues += len(public_entry["dialogues"])
        for item in public_entry["texts"]:
            grammar_counts[item["grammar"]] += 1
            choice_found |= item["text"] == "エミリアの質問に真面目に答える"
            ff42_found |= item["grammar"] == "inline-ff42" and item["text"] == "これはコメントです"
            ff8c_found |= item["grammar"] == "inline-ff8c" and item["text"] == "ぶりっじ"
        total_texts += len(public_entry["texts"])

    assert residual_fffe == residual_secondary == residual_inline == residual_long == 0
    assert short_raw_runs == [(87, [2, 1], [0xFFDE, 0, 0x3000])]
    assert total_dialogues == 20686
    assert total_pages == 37125
    assert total_texts == 121
    assert total_aliases == 5995
    assert grammar_counts == {"secondary-string": 119, "inline-ff42": 1, "inline-ff8c": 1}
    assert opening_line_found and choice_found and ff42_found and ff8c_found
    assert secondary_machine_suffix_guard
    assert [entry["entry_id"] for entry in dialogue["entries"][:6]] == [86, 8, 9, 10, 11, 12]

    compressed_size = bundle_path.stat().st_size
    dialogue_size = (root / "scenario-dialogue.json").stat().st_size
    assert compressed_size < dialogue_size // 4

    return {
        "entries": SC_COUNT,
        "dialogues": total_dialogues,
        "pages": total_pages,
        "additional_text_records": total_texts,
        "text_grammars": grammar_counts,
        "sparse_glyph_aliases": total_aliases,
        "state_contains_duplicate_text": False,
        "state_bundle_bytes": compressed_size,
        "dialogue_document_bytes": dialogue_size,
        "residual_fffe_in_raw_nodes": residual_fffe,
        "residual_secondary_strings_in_raw_nodes": residual_secondary,
        "residual_ff42_ff8c_strings_in_raw_nodes": residual_inline,
        "residual_unclassified_strings_3plus_glyphs": residual_long,
        "two_glyph_raw_sequences": {
            "count": len(short_raw_runs),
            "classification": "entry 87 FFDE comparison-expression operands, not text",
        },
        "opening_line_recovered": opening_line_found,
        "choice_text_recovered": choice_found,
        "inline_ff42_recovered": ff42_found,
        "inline_ff8c_recovered": ff8c_found,
        "secondary_machine_suffix_false_positive_guard": secondary_machine_suffix_guard,
        "default_debug_directory_present": False,
        "machine_state_files": 1,
        "dialogue_document": "scenario-dialogue.json",
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
