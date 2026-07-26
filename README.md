# rz-tool 1.0.0

`rz-tool` is an engine-specific extractor and rebuilder for the PCSG00933
resource set. The repository is split by responsibility:

- `cri-archive-lib` provides generic CPK parsing, ITOC/TOC ordering,
  compression and archive writing;
- `rz-assets` provides the game-specific package, texture, script, font and
  executable-patch contracts;
- `rz-tool` provides the command-line interface.

The project root is `rz-project.json` with `schema_version: 1`. Image-package,
script metadata, font, charset and routing documents use `document_version: 1`;
the editable script IR uses `document_version: 1` because dialogue bodies retain
all ordered `FFFE` page boundaries. A build directory is expected to be generated
by this extractor so that machine-managed source buffers, fingerprints and
metadata are available to the corresponding rebuild path.

## Supported resources

| Resource | Editable representation | Status |
|---|---|---|
| `sc.cpk` | engine-ID-named script/meta JSON, shared charset and route report | relocatable paged Unicode script editing; optional companion ELF patch when allocations grow |
| `addpt.cpk` | PNG + package JSON + machine-managed source buffers | editable for the proven package profile |
| `bk.cpk` | PNG + package JSON + machine-managed source buffers | editable for the proven BK profile |
| `bsf.cpk` | PNG + package JSON + machine-managed source buffers | editable for the proven streaming profile |
| `pt.cpk` | PNG + package JSON, or explicit opaque entry | mixed archive; proven entries are editable |
| `lt.bin` | atlas PNG + JSON | editable 3,602-glyph, 24×24, 4-bpp bank |
| `pr.bin` | raw-only | excluded from editable mode because the engine uses incompatible slot grammars |
| audio/video | rejected | outside scope |

## Build

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features
cargo test --workspace --all-features
cargo build --release -p rz-tool
```

The archive includes a Python source/ELF/Capstone validator, but Rust toolchain
checks and target runtime tests are part of the release checklist.

## CLI

```text
rz-tool extract <input.cpk|lt.bin> <project-directory>
rz-tool extract <input.cpk|lt.bin|pr.bin> <project-directory> --raw-only
rz-tool build <project-directory> <output.cpk|lt.bin|pr.bin>
rz-tool build <sc-project> <output.cpk> [--charset-map <charset.json>]
rz-tool build <sc-project> <output.cpk> \
  --eboot-in <eboot.bin.elf> --eboot-out <patched.bin.elf> \
  [--charset-map <charset.json>]
```

`--eboot-in` must be the analyzed 32-bit little-endian ELF, not an encrypted or
SELF-wrapped executable. Input and output ELF paths must be supplied together.
The input ELF is never overwritten.

## Project contract

Every project is anchored by `rz-project.json`. The manifest records the
project mode, original source name and the source-specific rebuild contract. A
CPK project stores the original archive profile (`alignment`, TOC/ITOC mode,
version, revision and update timestamp) plus one asset entry per original CPK
entry. Each asset entry preserves the original CPK directory, file name, ID and
user string, while its `asset_type` selects exactly one rebuild path.

Generated asset files are written at the project root. `rz-project.json`
separates `order` (CPK iteration/emission order) from `id` (the engine-visible
ITOC file ID). Image archives use `order` as their five-digit filename prefix.
`sc.cpk` uses `id`, because the VM passes that value unchanged to the scenario
loader; therefore `00086.script.json` means engine scenario ID 86 even when it is
the new-game entry point. Paths in the manifest must remain relative to the
project and may not escape the project directory.

### Editable image entries

A successfully decoded image-package entry is represented by one manifest asset
of type `engine-image-package` pointing to:

```text
00078.package.json
```

`NNNNN.package.json` is the authoritative engine-package document for that CPK
entry. It records the package profile, entry ID, fixed engine allocation size,
bundle mapping, preserved non-rebuilt layout bytes, descriptor/texture/palette
metadata, chunk plan and fingerprints for the machine-managed source buffers.
The document is edited only for structural metadata that is part of the declared
contract; visual edits are made in the referenced PNG files.

Each physical subpackage referenced by `NNNNN.package.json` uses a three-digit
subpackage suffix:

```text
00078-000.png
00078-000.decoded-source.bin
00078-000.chunk-table-source.bin
```

`NNNNN-SSS.png` is the editable RGBA view for physical subpackage `SSS`. For
bundle-backed entries, the document may reference `00078-001.png`,
`00078-002.png` and additional matching source-buffer pairs. The suffix is a
physical subpackage index, not a CPK entry ID.

`NNNNN-SSS.decoded-source.bin` is the exact aggregate destination buffer after
the engine's ordered GZIP writes. It is the source of truth for the original GPU
bytes, including block-compressed storage units, palette indices, padding and
any decoded range that is outside the user-visible PNG. Build uses it to decide
which physical blocks changed and to preserve non-visible edge texels.

`NNNNN-SSS.chunk-table-source.bin` is the exact original compressed table span:
the count/offset header, per-block output-size word, three reserved words, GZIP
streams and alignment bytes. Build reuses table bytes for chunks whose decoded
destination range is unchanged and recompresses only chunks whose destination
bytes changed.

The two `*-source.bin` files are machine-managed rebuild context. Their FNV-1a
fingerprints are stored in `NNNNN.package.json`; if they are edited, removed or
mismatched, the package is rejected rather than rebuilt from incomplete state.
They are not raw fallbacks and not user-editable assets.

### Explicit opaque image entries

When an entry belongs to a supported image archive but no proven editable
grammar accepts its bytes, the extractor records an explicit opaque asset:

```text
00102.opaque.bin
```

`NNNNN.opaque.bin` is the original entry payload preserved under a named project
asset. The manifest stores `asset_type: opaque` and a parser rejection reason.
Opaque entries are rebuildable byte-for-byte as part of the CPK, but they are
not texture-editable. For a given entry prefix, `NNNNN.package.json` and
`NNNNN.opaque.bin` are mutually exclusive.

### Script entries

An editable `sc.cpk` project creates two JSON documents per engine entry and two
shared project documents:

```text
NNNNN.script.json
NNNNN.script-meta.json
charset.json
scenario-routing.json
```

`NNNNN` is the engine-visible ITOC ID, not chronological play order.
`NNNNN.script.json` is the user-editable, source-equivalent IR. A dialogue node
contains its marker, `speaker`, `speaker_source_glyphs` and an ordered `pages`
array. Each page contains editable Unicode `text` plus machine-managed
`source_glyphs`. Every page corresponds to one glyph run terminated by `FFFE`;
retaining these boundaries is necessary because the VM advances at each
terminator. If Unicode remains unchanged, build reuses the original glyph IDs so
charset aliases remain byte-exact; edited text is encoded through `charset.json`.
Commands and data whose full opcode semantics are not proven remain exact raw
`u16` nodes.

`NNNNN.script-meta.json` is machine-managed rebuild metadata: allocation, voice
header, allocation footer, secondary relocation records, resource-routing
annotations and a reference back to the editable script JSON. The CPK manifest
points to the meta document because build needs both the editable IR and the
engine metadata.

`charset.json` maps Unicode codepoints to the existing glyph IDs accepted by the
engine and is the default charset map used by `rz-tool build` unless
`--charset-map` supplies a different document.

`scenario-routing.json` is a derived navigation report and is not consumed by
build. It records the mapping between archive `order` and engine `entry_id`, the
proven new-game root `86:0`, and range-validated cross-entry `FFEF` route candidates. The
scenario is a branching graph, so the file intentionally does not invent a
single sequential renumbering.

### `lt.bin` projects

An editable `lt.bin` project contains:

```text
lt-font.json
lt-atlas.png
```

`lt-atlas.png` is the editable glyph atlas. `lt-font.json` records the fixed
font geometry, atlas path, atlas column count and tail policy. The bytes
`0xfd440..0xfd7ff` are stored inline as `tail_hex` when exact binary round-trip
is requested; extraction does not create a separate `lt-tail.bin`.

### Raw-only projects

`--raw-only` disables editable decoding. For `lt.bin` and `pr.bin`, the project
contains `source.bin` and a `raw-file` manifest source. For CPK input, each
entry is written as a `raw` manifest asset named from the entry order and a
sanitized source filename. Raw-only projects rebuild by copying those project
assets back through the corresponding CPK or raw-file writer without applying
engine-specific editing semantics.

## Editable texture rebuild

The verified engine sequence is:

```text
select compressed table
→ read block start/end
→ inflate block+0x10 into output + index*stride
→ after the final block, install metadata
→ copy texture bytes
→ copy palette bytes
```

Build follows that contract:

1. verify the package profile, descriptor, preserved decoded buffer and source
   chunk table;
2. decode source GPU bytes to obtain the visual baseline;
3. encode the edited PNG into the original physical layout;
4. preserve unchanged BC storage blocks byte-for-byte;
5. re-encode changed BC1/BC2/BC3 blocks with source endpoints as seeds and keep
   non-visible edge texels;
6. rebuild P4/P8 palettes and indices automatically, preserving stable source
   indices where possible and using deterministic median-cut quantization when
   the image exceeds 16/256 colors;
7. recompress only destination chunks whose bytes changed;
8. reparse the rebuilt package, inflate every block and require exact aggregate
   GPU-buffer equality with the intended encoded buffer;
9. decode the rebuilt texture and validate reversible or lossy-format
   invariants before CPK packing.

Supported lossy formats are editable by default. There is no opt-in flag and no
copy-only mode disguised as safety.

## `sc.cpk`

Extraction names each entry by its engine-visible ITOC ID and creates:

```text
NNNNN.script.json       editable source-equivalent IR
NNNNN.script-meta.json  machine-managed engine metadata
scenario-routing.json   derived route graph for navigation
```

The executable starts a new game at entry `0x56` (86), stream 0. Consequently,
`00086` is the correct engine identity of the prologue; renaming it to `00000`
would invalidate script transitions and executable references. `order` remains
in `rz-project.json` solely for CPK emission.

The editable document contains labels, Unicode dialogue pages, original glyph-ID
vectors and exact raw `u16` nodes for unclassified commands/data. Node order is
physical payload order, not a promised playthrough order; `stream_XXXX` labels
and VM branches select the executed path. `marker_index` is the ordered primary
marker-table index inside one entry, not a global chronological line number. A
dialogue is emitted as one speaker followed by one or more ordered pages, each
ending in `FFFE`. Unchanged spans reuse their source glyph IDs; edited spans are
encoded from Unicode. The meta document
contains allocation, voice header, footer, secondary relocation records and
engine annotations. There are no script skeleton binaries.

Build regenerates:

```text
+0x0000..+0x007f  voice block
+0x0080            runtime header
+0x0090            primary marker offsets
                    0x10 alignment
                    secondary records (7 × u32)
                    zero padding to +0x2000
+0x2000            stream-offset table + relocated payload
```

Unicode edits may grow or shrink. The assembler resolves labels, regenerates
stream/primary tables, relocates corpus-confirmed secondary payload addresses,
restores the 16-byte allocation-end footer, pads unused capacity, and reparses
the result.

When rebuilt entries exceed stock sector allocations, provide `--eboot-in` and
`--eboot-out`. The patcher signature-checks and updates:

| Virtual address | Data |
|---:|---|
| `0x8111344c` | 89 SC `(start_sector, sector_count)` records |
| `0x810f9b1c` | 89 stream/primary/secondary global base-count records |
| `0x8101b554` | supported Thumb immediate for the runtime buffer size |

## Charset and `lt.bin`

The engine renderer accepts glyph IDs below `0x0e12`, addresses each glyph at
`glyph_id × 0x120`, and expands 12 packed bytes per row for 24 rows, low nibble
before high nibble. `lt.bin` therefore contains 3,602 glyphs of 24×24 pixels at
4 bits per pixel.

`charset.json` maps Unicode to those existing glyph IDs. Editing glyph shapes
requires rebuilding `lt.bin`; remapping characters requires a matching charset
and font edit. Adding IDs beyond `0x0e11` is not supported.

## Evidence and validation

`ENGINE_ANALYSIS.md` is the single technical analysis report. Raw reproducible
disassembly evidence remains in `CAPSTONE_EVIDENCE.txt` and
`SCRIPT_VM_XREF_EVIDENCE.txt`. `secrect.json` is the external function-address
map consumed by `tools/research/analyze_engine_assets.py` when regenerating the
Capstone evidence; it is an analysis input, not a project artifact generated by
`rz-tool`.

Run the stable validator with the supplied analysis inputs:

```bash
python tools/validate_engine_assets.py \
  --elf /path/to/eboot.bin.elf \
  --secrect /path/to/secrect.json \
  [--capstone-path /path/to/capstone_pkg] \
  [--capstone-wheel /path/to/capstone-5.0.9-*.whl] \
  --output VALIDATION.json
```

The validator checks the source-level project contract, the 3,602-codepoint
charset table, the source-aware texture rebuild contracts, executable sector
tables, SC metadata records, ELF patch signatures, and byte-identical Capstone
evidence regeneration. A representative `sc.cpk` project may additionally be
supplied with `--corpus`.

Runtime validation covers no-edit and edited rebuilds for `addpt.cpk`,
`bk.cpk`, `bsf.cpk`, `pt.cpk`, `sc.cpk` and `lt.bin`; re-extraction comparison;
BC edge-block edits; P4/P8 palette overflow; compressed-table relocation; SC
capacity growth with companion ELF patching; and emulator or hardware scene
coverage.

See `VALIDATION.md` for the release checklist.
