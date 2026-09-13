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

- workspace version remains whatever is declared in Cargo.toml; project schema version `1`;
- `scenario-dialogue.json` as the authoritative SC editing document;
- one compact machine rebuild bundle at `.rz-internal/sc-build-state.json.gz`, document version `1`;
- no root-level per-entry script IR/state files during normal extraction;
- no Unicode text, speaker/page strings, or complete source glyph vectors in the machine bundle;
- only sparse non-canonical glyph aliases are retained for byte-exact no-edit rebuild;
- optional diagnostic copies only through `--debug-script-ir`;
- exact `(entry_id, marker_index)` dialogue and `(entry_id, text_index)` text overlay validation during build, including rejection of empty non-dialogue text records;
- fail-closed runtime coverage for `FFFE` pages, secondary-target strings and `FF42`/`FF8C` inline strings; low-valued `FFFF`-terminated operand runs are preserved as machine data unless an engine-proven text anchor owns them;
- extraction-time byte-exact IR reassembly, including secondary relocation records;
- exact SC integrity-footer verification and regeneration matching `FUN_8102B4AC`:
  two little-endian wrapping `u64` lane sums seeded with `0x1111111111111111`;
- the same common-reader footer regeneration in the image-package encoder,
  performed after fixed-allocation padding for ADDPT/BK/BSF/PT;
- a generic `tools/cpk_integrity.py` repair/check path for archives emitted
  by older builds with stale image-entry footers;
- source verification and footer regeneration for standalone `lt.bin`, plus
  `tools/lt_integrity.py` for files built by older atlas encoders;
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
   contains only `rz-project.json`, `scenario-dialogue.json`, and
   `.rz-internal/sc-build-state.json.gz`.
2. Extract with `--debug-script-ir` and confirm diagnostic copies appear only
   under `debug/scenario-ir/`.
3. Confirm normal extraction produces one `.rz-internal/sc-build-state.json.gz`
   bundle and no root-level routing, charset, allocation-map, dialogue-metadata
   or per-entry IR/state documents.
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

## BK texture integrity regression

The supplied `bk.cpk` regression was executed with entry `325` (`00325-000.png`):

1. stock archive: all 326 fixed allocations match `FUN_8102B4AC`;
2. no-edit rebuild: entry 325 is byte-identical and all 326 footers match;
3. one-pixel edit using the old serializer: package/GZIP reparses and remains
   within `0x111000`, but footer 325 is stale and only 325/326 entries validate;
4. footer regeneration: all 326 entries validate and re-extraction reproduces
   the edited 1024x544 RGBA image exactly.


## LT atlas integrity regression

The supplied `lt.bin.org` regression verifies the standalone startup loader:

1. the file is exactly `0xFD800` bytes (`0x1FB` sectors);
2. glyph data occupies `0xFD440` bytes and the final `0x10` bytes are the
   `FUN_8102B4AC` footer;
3. the stock footer `71f70d807e60ce6c9129df8e1a01f723` reproduces exactly;
4. no-edit rebuilding with the previous encoder is byte-identical;
5. changing atlas pixel `(0,0)` changes one glyph byte but leaves a stale footer
   in the previous build, so verification fails;
6. regenerating the footer changes `71` to `80` at offset `0xFD7F0`, verification
   passes, and re-extraction preserves the edited pixel.

## Wrap materialization validation

For VWF text builds, validate that wrapping is serialized, not only computed
locally. `--eboot-in` must be a standalone VWF-patcher output; stock EBOOT with
`--charset-map` or `--wrap-width-table` must be rejected before any deployable
EBOOT is written. The VWF input check is the SHA-256 of virtual range
`0x81000000..0x81100000`:

```text
c7cc66521264acdc6d259ad189d1a6fe731901855a6708ae3a3b036334091a84
```

`rz-tool build lt.bin` must require `--eboot-in`/`--eboot-out`, verify that `lt-font.json.glyph_count` equals the EBOOT runtime glyph limit, and update only the LT load-size/sector allocation constants. `extract-lt-alloc` must recover that allocation plus the runtime glyph limit for modified-LT re-extraction. `extract-sc-alloc` remains the SC-specific allocation-map command.

When an EBOOT has intentionally already been changed inside the protected hash range by an LT or SC allocation build, `-f`/`--force` may bypass the hash mismatch for the next build. Force must not bypass LT glyph-count/allocation consistency checks.

Representative workflow:

```bash
rz-tool extract sc.cpk.org sc_work --charset-map examples/font.tbl
# edit scenario-dialogue.json text field
rz-tool build sc_work sc_rebuilt.cpk \
  --charset-map examples/font.tbl \
  --wrap-width-table examples/font.cnf \
  --wrap-width-px 528 \
  --wrap-mode word \
  --eboot-in eboot_vwf.bin.elf \
  --eboot-out eboot_vwf_sc.bin.elf
rz-tool extract sc_rebuilt.cpk sc_verify --charset-map examples/font.tbl --debug-script-ir
```

The verification target is the raw script IR: every generated runtime message
entry must contain at most three `FFFE`-terminated rows/pages, and overflow must
appear as generated `FFF0` markers with cloned simple `FFFB FF68 <marker> <arg>`
invocations. Extra fourth/fifth rows under one marker are invalid because they
overwrite existing row slots at runtime.

## Continuation metadata validation

1. Build an SC edit that produces at least two generated continuation markers
   and at least one semantic row joiner.
2. Confirm build stages both `sc.cpk` and `sc.cpk.rz-dialogue-meta.json`, does
   not overwrite either path, and rolls back already-renamed outputs when a later
   in-process rename fails.
3. Inspect the companion and confirm it contains no dialogue text, row text,
   speaker text, wrap width, rows-per-screen, pixel metrics, raw words, or debug
   lines.
4. Confirm `archive_sha256` equals the rebuilt `sc.cpk` hash.
5. Re-extract with the companion adjacent to the archive. Confirm generated
   physical markers fold into one logical marker and later marker indices return
   to their pre-insertion logical values.
6. Confirm `.rz-internal/sc-build-state.json.gz` retains only structural
   dialogue metadata: row joiners and generated-marker counts, never full text.
7. Rebuild the folded project with the same wrap profile. Confirm no-edit text
   preserves the physical generated marker chain and changed text safely changes
   the chain length while remapping later `FF68` references.
8. Rename or remove the companion and re-extract. Confirm `rz-tool` does not
   guess continuation relationships from adjacency.
9. Pair the companion with a different SC archive. Confirm extraction rejects
   the SHA-256 mismatch before writing the project.
