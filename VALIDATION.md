# Validation for rz-tool 1.0.1

## Static, corpus and ELF validator

```bash
python tools/validate_engine_assets.py \
  --corpus /path/to/extracted-sc-project \
  --elf /path/to/eboot.bin.elf \
  --sc-cpk /path/to/original-sc.cpk \
  --secrect /path/to/mapper.json \
  [--capstone-path /path/to/capstone_pkg] \
  [--capstone-wheel /path/to/capstone-5.0.9-*.whl] \
  --output VALIDATION.json
```

The source checks require:

- workspace version `1.0.1` and project schema version `1`;
- `scenario-dialogue.json` as the authoritative SC editing document;
- one compact machine rebuild bundle at `.rz-internal/sc-state.json.gz`, document version `2`;
- no root-level per-entry script IR/state files during normal extraction;
- no Unicode text, speaker/page strings, or complete source glyph vectors in the machine bundle;
- only sparse non-canonical glyph aliases are retained for byte-exact no-edit rebuild;
- optional diagnostic copies only through `--debug-script-ir`;
- exact `(entry_id, marker_index)` dialogue and `(entry_id, text_index)` text overlay validation during build, including rejection of empty non-dialogue text records;
- fail-closed runtime coverage for `FFFE` pages, secondary-target strings and `FF42`/`FF8C` inline strings; low-valued `FFFF`-terminated operand runs are preserved as machine data unless an engine-proven text anchor owns them;
- extraction-time byte-exact IR reassembly, including secondary relocation records;
- exact SC integrity-footer verification and regeneration matching `FUN_8102B4AC`:
  two little-endian wrapping `u64` lane sums seeded with `0x1111111111111111`;
- source-aware BC encoding, automatic P4/P8 palette rebuild, incremental GZIP
  chunk reuse and rebuilt-package verification;
- the complete 3,602-codepoint charset table;
- exactly one analysis Markdown file, `ENGINE_ANALYSIS.md`.

The supplied SC corpus and original `sc.cpk` checks require:

- all 89 engine entries;
- 20,686 primary dialogues and 37,125 ordered pages;
- no `FFFE` page terminator remaining in a raw IR node;
- 121 additional proven text records: 119 `secondary-string`, one `inline-ff42`
  and one `inline-ff8c`;
- no proven secondary-target or `FF42`/`FF8C` text remaining in raw IR;
- the offline audit reports no unclassified `FFFF`-terminated standalone raw run of three or more glyph-range words; this numerical pattern is not a runtime grammar;
- entry 17 text record 6 preserves machine suffix `0004 0019 000e ffff` without reclassifying it as text;
- the only two-glyph raw candidate is entry 87's `FFDE 0000 3000 0002 0001 FFFF` comparison expression, which `FUN_8100b2ac` passes to `FUN_8100b134` as command operands rather than display text;
- all 89 original SC integrity footers reproduce exactly from their allocated entry bytes;
- the opening line `これ、本当にラムたちが` at entry 86, marker 1;
- the choice label `エミリアの質問に真面目に答える` and both inline strings
  present in `scenario-dialogue.json`;
- no default debug directory and exactly one compact machine-state file;
- 5,995 sparse glyph aliases replace the duplicated 599,524-glyph source vectors;
- the reference state bundle is 224,948 bytes compressed versus 4,658,919 bytes for the sole text document.

ELF checks compare executable sector tables with `engine_allocations.rs`, verify
the cumulative SC layout, validate all 89 global metadata records, and confirm
the stock runtime-buffer instruction at `0x8101b554`.

Capstone validation extracts the supplied wheel to a temporary directory,
regenerates `CAPSTONE_EVIDENCE.txt`, and requires byte-identical output.

## Rust release gates

The preparation environment does not contain a Rust toolchain. These commands
remain mandatory before distributing a compiled executable:

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features
cargo test --workspace --all-features
cargo build --release -p rz-tool
```

## Required SC integration matrix

1. Extract stock `sc.cpk` without `--debug-script-ir` and confirm the root
   contains only `rz-project.json`, `scenario-dialogue.json`,
   `scenario-routing.json`, `charset.json`, and `.rz-internal/`.
2. Extract with `--debug-script-ir` and confirm diagnostic copies appear only
   under `debug/scenario-ir/`.
3. Confirm normal extraction produces one `.rz-internal/sc-state.json.gz`
   bundle and no per-entry IR/state documents.
4. Decompress the state bundle and confirm its source nodes contain no `text`,
   `speaker`, `pages`, `source_glyphs`, or `speaker_source_glyphs` fields; only
   sparse `glyph_aliases`/`speaker_glyph_aliases`/`page_glyph_aliases` may refer
   to text positions.
5. Build without edits and compare all 89 logical payloads against the source.
6. Edit speaker text, page text, page count, secondary-target strings and both
   inline string classes through `scenario-dialogue.json` only.
7. Verify unchanged duplicate glyph aliases remain byte-identical.
8. Verify missing, duplicate, unknown and grammar-mutated marker/text keys are
   rejected, along with `FF42` text over 21 glyphs and `FF8C` text over 7.
9. Feed corpora containing an `FFFE` outside dialogue, a secondary-target string
   left raw, or a proven `FF42`/`FF8C` string left raw; confirm extraction fails.
10. Feed standalone command data containing three low-valued operands followed by
   `FFFF`; confirm it remains raw and round-trips instead of being misclassified.
11. Confirm entry 17 text record 6, whose typed machine suffix is
   `0004 0019 000e ffff`, extracts and rebuilds without being treated as hidden text.
12. Exercise relocation of stream, primary and secondary address classes.
13. Test growth beyond stock allocation with companion ELF output, then re-open
    the generated ELF and verify all three patched regions.
14. Boot affected scenes in an emulator and, where possible, target hardware.
