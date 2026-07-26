# Validation for rz-tool 1.0.0

## Stable static and ELF validator

```bash
python tools/validate_engine_assets.py \
  --elf /path/to/eboot.bin.elf \
  --secrect /path/to/secrect.json \
  [--capstone-path /path/to/capstone_pkg] \
  [--capstone-wheel /path/to/capstone-5.0.9-*.whl] \
  --output VALIDATION.json
```

Optional SC corpus validation:

```bash
python tools/validate_engine_assets.py \
  --corpus /path/to/sc-project \
  --elf /path/to/eboot.bin.elf \
  --secrect /path/to/secrect.json \
  [--capstone-path /path/to/capstone_pkg] \
  [--capstone-wheel /path/to/capstone-5.0.9-*.whl] \
  --output VALIDATION.json
```

The source checks require:

- workspace version `1.0.0`;
- project schema, script metadata, routing and non-script document versions `1`;
- editable script document version `1`, with ordered pages and source glyph IDs;
- no compatibility flag/field and no migration scripts;
- exactly one analysis Markdown file, `ENGINE_ANALYSIS.md`;
- source-aware BC encoding, automatic P4/P8 palette rebuild, incremental GZIP
  chunk reuse and rebuilt-package verification;
- the complete 3,602-codepoint charset table.

ELF checks compare executable sector tables with `engine_allocations.rs`, verify
the cumulative SC layout, validate all 89 global metadata records, and confirm
the stock runtime-buffer instruction at `0x8101b554`.

Capstone validation extracts the supplied wheel to a temporary directory,
regenerates `CAPSTONE_EVIDENCE.txt`, and requires byte-identical output.

## Rust release gates

These commands were not runnable in the preparation environment and must pass
before release:

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features
cargo test --workspace --all-features
cargo build --release -p rz-tool
```

## Required integration matrix

For `addpt.cpk`, `bk.cpk`, `bsf.cpk` and `pt.cpk`:

1. extract with 1.0.0;
2. build without edits;
3. re-extract the rebuilt CPK;
4. compare package metadata, source decoded buffers and visual output;
5. test intentional RGBA edits;
6. test P4/P8 edits within and above palette capacity;
7. test BC1/BC2/BC3 single-block, multi-block, edge-block and full-surface edits;
8. force compressed-table relocation;
9. boot affected scenes in an emulator and, where possible, target hardware.

For `sc.cpk`:

1. no-edit rebuild of all 89 entries;
2. verify 20,686 dialogues and 37,125 ordered pages;
3. verify entry ID 86, stream 0 as the startup route and all range-validated `FFEF` route candidates;
4. verify unchanged duplicate glyph aliases remain byte-identical;
5. variable-length dialogue growth and shrinkage;
6. relocation of stream, primary and secondary address classes;
7. growth beyond stock allocation with companion ELF output;
8. re-open the generated ELF and verify all three patched regions;
9. exercise scenes using changed entries.

For `lt.bin`:

1. no-edit exact rebuild with preserved tail;
2. intentional glyph edit;
3. zero-fill-tail policy test;
4. matching `charset.json` remap test.
