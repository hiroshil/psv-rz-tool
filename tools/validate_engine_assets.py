#!/usr/bin/env python3
"""Static source validator for the clean VWF EBOOT input contract."""
from __future__ import annotations

import json
import pathlib

ROOT = pathlib.Path(__file__).resolve().parents[1]


def read(rel: str) -> str:
    return (ROOT / rel).read_text(encoding="utf-8")


def main() -> None:
    cargo = read("Cargo.toml")
    script = read("crates/rz-assets/src/codec/script.rs")
    pipeline = read("crates/rz-assets/src/pipeline.rs")
    eboot = read("crates/rz-assets/src/eboot.rs")
    main_rs = read("crates/rz-tool/src/main.rs")
    validation = json.loads(read("VALIDATION.json"))

    assert 'version = "1.0.1"' in cargo

    assert "const DOCUMENT_VERSION: u32 = 1" in script
    assert "const SOURCE_DOCUMENT_VERSION: u32 = 1" in script
    assert "const ROUTING_DOCUMENT_VERSION: u32 = 1" in script
    assert "const DIALOGUE_DOCUMENT_VERSION: u32 = 1" in script
    assert "const STATE_BUNDLE_VERSION: u32 = 1" in script
    assert 'const STATE_BUNDLE_PATH: &str = ".rz-internal/sc-build-state.json.gz"' in script
    assert 'const LEGACY_STATE_BUNDLE_PATH: &str = ".rz-internal/sc-state.json.gz"' in script
    assert "routing: Some(routing_document)" in script
    assert "dialogue_metadata: Some(dialogue_metadata_document)" in script

    forbidden_project_outputs = [
        'scenario-dialogue.meta.json',
        'scenario-routing.json',
        'sc-allocation.json',
        'charset.json',
        'charset-map.source',
    ]
    for name in forbidden_project_outputs:
        assert f'join("{name}")' not in script

    assert "VWF_RUNTIME_HASH_RANGE_START_VA" in eboot
    assert "VWF_RUNTIME_HASH_RANGE_END_VA" in eboot
    assert "VWF_RUNTIME_HASH_RANGE_SIZE" in eboot
    assert "VWF_RUNTIME_HASH_RANGE_SHA256" in eboot
    assert "0x8100_0000" in eboot
    assert "0x8110_0000" in eboot
    assert "8eac77d2ff46522e7c428bc8231b88b8606a5dc4f2608765955266cf06666ac9" in eboot
    assert "validate_vwf_runtime_patch" in eboot
    assert "require_vwf_runtime" in eboot
    assert "run the standalone VWF patcher first" in eboot

    assert "require_vwf_runtime" in pipeline
    assert "load_font_width_config" in pipeline
    assert "strip_font_config_group_comment" in pipeline
    assert ".rz-internal/sc-build-state.json.gz" in pipeline
    assert ".rz-internal/sc-build-state.json.gz" in main_rs

    legacy_width_name = "fonttbl_width" + "_buckets.tbl"
    assert (ROOT / "examples/font.cnf").exists()
    assert not (ROOT / "examples" / legacy_width_name).exists()
    font_cnf = read("examples/font.cnf")
    assert "[advance_groups]" in font_cnf
    assert "8=<space>" in font_cnf
    assert "16=#AOQ" in font_cnf
    assert legacy_width_name not in read("README.md")
    assert legacy_width_name not in read("VALIDATION.md")

    assert validation["source"]["workspace_version"] == "1.0.1"
    assert validation["source"]["scenario_routing_version"] == 1
    assert validation["source"]["scenario_state_bundle_version"] == 1
    assert validation["source"]["scenario_state_bundle_path"] == ".rz-internal/sc-build-state.json.gz"
    assert validation["source"]["font_width_config"] == "examples/font.cnf"
    assert validation["vwf_eboot_input_contract"]["stock_eboot_with_vwf_wrap_must_reject"] is True
    assert validation["vwf_eboot_input_contract"]["standalone_vwf_patcher_output_required_for_charset_or_wrap"] is True
    assert validation["vwf_eboot_input_contract"]["runtime_hash_range_va"] == "0x81000000..0x81100000"
    assert validation["vwf_eboot_input_contract"]["runtime_hash_range_sha256"] == "8eac77d2ff46522e7c428bc8231b88b8606a5dc4f2608765955266cf06666ac9"

    docs = "\n".join(read(name) for name in ["README.md", "ENGINE_ANALYSIS.md", "VALIDATION.md"])
    forbidden_doc_markers = [
        "S" + "tep",
        "S" + "TEP",
        "RZ_TOOL_" + "ST" + "EP",
        "USAGE_WRAP_WORKFLOW",
        "fix_cpk_integrity.py",
        "fix_lt_integrity.py",
    ]
    for marker in forbidden_doc_markers:
        assert marker not in docs, marker
    legacy_prefix = "st" + "ep"
    forbidden_validation_keys = [key for key in validation if key.lower().startswith(legacy_prefix)]
    assert not forbidden_validation_keys, forbidden_validation_keys

    print("static validation passed")


if __name__ == "__main__":
    main()
