# PCSG00933 engine analysis for rz-tool 1.0.1

## 1. Purpose and claim boundary

This report is the single authoritative analysis document for `rz-tool 1.0.1`.
It consolidates the resource-registry, CPK/ITOC, image-package, texture, script,
font and executable-patch findings used by the implementation.

The report distinguishes four evidence classes:

- **Direct instruction evidence:** a mapped routine reads, writes, compares or
  passes the stated value.
- **Call-chain evidence:** ownership or sequencing follows from identified
  callers and callees.
- **Corpus-confirmed grammar:** a proposed structure matches every supplied
  entry and independent executable counts.
- **Preservation boundary:** the engine consumes bytes whose producer semantics
  are not sufficiently proven; those bytes are retained explicitly rather than
  regenerated from a guess.

The analysis does not claim recovery of original source-language syntax,
comments, macro names, local identifiers, all script opcode semantics, or all
`pr.bin` slot formats. Where those facts are absent from the executable and
corpus, the implementation remains structural and lossless rather than
inventing semantics.

## 2. Inputs and reproducibility

The stable analysis was based on the supplied files:

| Input | SHA-256 |
|---|---|
| `eboot.bin.elf` | `f5233a9ed15bedab8893800701a176da07986952fb0fb9ce92b33519327e1825` |
| Capstone 5.0.9 wheel | `273fd8d747d2e35c88f91450be51a603ecfaafb00d96d9f315dcb8689c86193e` |
| `mapper.json` | `1d31d79ad9dc9ff8e64e57f5fa395317106d5d42adab7ae69821ffea45ad503e` |
| `output.c` | `5c2d5f02636b2df020eb9e48798e56fdf8b950cd266ed3d0f6513cdba89fd3f4` |
| `output.asm` | `aac4aa99aa1a4ed7d3918884c5795ace9c779a43717797258001732b23bec61d` |
| generated `CAPSTONE_EVIDENCE.txt` | `1b0fa147b016ecfa1f8d302e3cb33685343883b0a4147cef7a2aa6046c29a609` |

Capstone is imported by extracting the wheel; installing it into the Python
environment is not required:

```bash
mkdir capstone-wheel
unzip capstone-5.0.9-*.whl -d capstone-wheel
PYTHONPATH=capstone-wheel \
python tools/research/analyze_engine_assets.py \
  eboot.bin.elf mapper.json > CAPSTONE_EVIDENCE.txt
```

`tools/validate_engine_assets.py` can regenerate the report and require
byte-for-byte equality with the checked-in evidence.

## 3. Architecture and ownership

`rz-tool` uses a strict layering boundary:

```text
cri-archive-lib
  generic CPK/UTF/ITOC mechanics and archive writing

rz-assets
  PCSG00933 allocations, package grammars, textures, script VM IR,
  font bank, validation and ELF patching

rz-tool
  CLI and transaction orchestration
```

Engine codecs are not merged into `cri-archive-lib`. This prevents generic CPK
code from silently acquiring title-specific assumptions.

The static resource registry begins at `0x8112a2e0`:

| Index | Resource | Declared count | Stable policy |
|---:|---|---:|---|
| 0 | `vo.awb` | `0x4bd0` | audio, excluded |
| 1 | `sc.cpk` | `0x59` | editable compiled scripts |
| 2 | `addpt.cpk` | `1` | editable image package/bundle |
| 3 | `snd.cpk` | `0x1b0` | audio, excluded |
| 4 | `bk.cpk` | `0x146` | editable image packages |
| 5 | `bsf.cpk` | `0x126` | editable streaming image packages |
| 6 | `pt.cpk` | `0x17` | mixed image packages; unproved entries become opaque |
| 7 | `lt.bin` | `1` | editable glyph bank |
| 8 | `pr.bin` | `1` | raw-only; mixed grammars |
| 9 | `se.awb` | `1` | audio, excluded |

`FUN_810224bc` installs only the first seven records into the CRI binder:

```text
810224ec  movw r1, #0xa2e0
810224f2  movt r1, #0x8112
810224fc  movs r0, #7
810224fe  str  r0, [r4, #0x14]
```

The adjacent standalone table at `0x81129e58` describes:

| ID | File | Size | Sectors |
|---:|---|---:|---:|
| 7 | `lt.bin` | `0x0fd800` | `0x01fb` |
| 8 | `pr.bin` | `0x105f800` | `0x20bf` |
| 9 | `se.awb` | `0x07c800` | `0x00f9` |

`lt.bin` and `pr.bin` are therefore not CPK entries accidentally missed by the
tool; they are independently allocated and loaded resources.

## 4. CPK and ITOC behavior

### 4.1 `FUN_81066e32`: ITOC subtable search

The routine binary-searches one ITOC width-class table and returns the matching
row or insertion position.

### 4.2 `FUN_8106706c`: combined DataL/DataH lookup

The routine searches both ITOC width classes by file ID. `DataL` and `DataH` are
index encodings for different field widths, not separate physical payload
areas.

### 4.3 `FUN_81066ed2`: physical prefix calculation

The routine sums aligned sizes for rows with IDs preceding the requested ID
across both subtables. The physical payload order is therefore the merged order
by file ID. The patched `cri-archive-lib` reader/writer must preserve that
merged order; concatenating all `DataL` rows followed by all `DataH` rows is
incorrect.

### 4.4 Fixed executable allocation tables

The executable contains sector-count tables independent of CPK metadata:

| Archive | Address | Records | Record stride | Sector field |
|---|---:|---:|---:|---:|
| `addpt.cpk` | `0x811122dc` | 1 | 4 | `+2`, `u16` |
| `pt.cpk` | `0x81113394` | 23 | 4 | `+2`, `u16` |
| `sc.cpk` | `0x8111344c` | 89 | 4 | `+2`, `u16` |
| `bk.cpk` | `0x810fa500` | 326 | 8 | `+4`, `u16` |
| `bsf.cpk` | `0x810faf30` | 294 | 8 | `+4`, `u16` |

Rebuilt logical data must fit the corresponding fixed allocation unless the
specific engine table is patched. `rz-tool` patches only the analyzed SC
allocation path. ADDPT/PT/BK/BSF output exceeding stock allocation is rejected.

## 5. Common entry reader and allocation model

`FUN_81053b7a` converts a sector count into bytes by shifting left 11 bits,
therefore multiplying by `0x800`. Callers supply archive identity, entry ID,
destination and the executable-resident sector count.

This establishes two independent constraints:

1. CPK metadata must describe the rebuilt file correctly.
2. The executable allocation table must permit the same number of sectors.

A generic CPK writer cannot solve an engine allocation overflow by itself.

`FUN_8101b504` allocates the major standalone/runtime buffers, including:

- `0x0fd800` for `lt.bin`;
- `0x105f800` for `pr.bin`;
- `0x07c800` for `se.awb`;
- stock `0x00020000` for the active SC entry;
- `0x01800000` scratch capacity used by package loading paths.

## 6. Image-package grammar

### 6.1 Shared loader: `FUN_81053694`

The shared loader processes one compressed destination block per invocation:

1. read `u32(package + 0x34)` to locate the compressed region;
2. add a caller-selected table delta (`0` or `0x1400` in analyzed profiles);
3. read the table count word at relative `+0`;
4. read block start/end offsets from the `u32` array beginning at `+4`;
5. compute destination as `output_base + active_index * u32(package + 0x3c)`;
6. invoke the GZIP block wrapper;
7. after the final block, install supplemental metadata, texture bytes and
   palette bytes.

Selected instructions:

```text
8105369c  ldr    r0, [r6, #0x34]
810536a4  adds   r0, r6, r0
810536ae  add.w  sb, r0, r2
810536b2  ldr.w  sl, [r0, r2]
810536ba  ldr.w  fp, [r0, #4]
810536d4  ldr    r2, [r0, #8]
810536da  ldr    r3, [r6, #0x3c]
810536dc  mla    r1, r1, r3, r7
8105374c  bl     0x810225fa
81053764  bl     0x8102fdca
8105376e  bl     0x8102fd82
```

The loop behaves as a do-while: stored counts zero and one both cause block zero
to execute once. The active block index is byte-sized, so the editable model
rejects plans above 255 blocks. BSF uses only the low byte of its count word;
the upper 24 bits remain opaque and are preserved.

### 6.2 Package header fields

Directly consumed fields include:

| Package offset | Meaning proven by consumer |
|---:|---|
| `+0x04` | offset to direct `0x30`-byte texture descriptor |
| `+0x10` | supplemental record count |
| `+0x14` | supplemental record table offset |
| `+0x24` | palette/auxiliary offset |
| `+0x34` | compressed region offset |
| `+0x3c` | destination stride per block |

Bytes with incomplete producer semantics remain visible in package JSON through
preserved layout fields. `rz-tool` does not hide them in a skeleton file.

### 6.3 GZIP block wrapper: `FUN_81034e4a`

```text
81034e5a  adds.w r2, r0, #0x10
81034e5e  ldr    r4, [r0]
81034e66  bl     0x810e34c4
```

The block layout is:

```text
+0x00  u32 decompressed_size
+0x04  u32 opaque/reserved_0
+0x08  u32 opaque/reserved_1
+0x0c  u32 opaque/reserved_2
+0x10  RFC 1952 GZIP stream
```

The three intermediate words are engine-observed metadata without a proven
producer grammar. They are retained for unchanged blocks. For changed blocks,
the size and GZIP stream are rebuilt while the reserved words remain unchanged.

### 6.4 Bundle loader: `FUN_81053fa6`

ADDPT/PT bundle records use:

```text
+0x00  u8 package_count
+0x04  u32 package_relative_offset[package_count]
```

The loader adds each offset to the bundle base and dispatches the resulting
subpackage to `FUN_81053694`.

### 6.5 Supplemental records: `FUN_810225fa`

The routine copies `count × 0x20` bytes into runtime state and enforces a fixed
pool limit. Record extent and count are direct evidence. Individual semantic
field names are not fully recovered, so the exact records remain authoritative.

## 7. Texture descriptor and GPU storage

### 7.1 Direct descriptor

The analyzed path uses a `0x30`-byte descriptor. Fields consumed by the stable
codec include:

| Descriptor offset | Field |
|---:|---|
| `+0x04` | texture data size |
| `+0x10` | runtime GXT pointer; persisted direct form is zero |
| `+0x14` | palette byte size (`0x40` P4, `0x400` P8) |
| `+0x18` | allocation/address flags |
| `+0x1c` | texture type |
| `+0x20` | texture format |
| `+0x28` | width (`u16`) |
| `+0x2a` | height (`u16`) |
| `+0x2d` | memory-bank selector written during loading |

Unclassified bits remain preserved. PNG cannot reconstruct them.

### 7.2 Texture copy: `FUN_8102fdca`

The routine derives byte width from the descriptor and copies the post-GZIP
source buffer directly into mapped GPU memory. It does not decode an image or
canonicalize storage. Observed paths include P4, P8, selected three-byte formats
and four-byte RGBA storage, plus BC formats handled as physical GPU blocks.

### 7.3 Palette copy: `FUN_8102fd82`

The routine accepts palette sizes `0x40` and `0x400` and copies palette bytes in
order. Duplicate colors at different indices remain observable; palette order
is not a disposable set.

### 7.4 Runtime RGBA surfaces

Executable descriptors prove profile-specific runtime surfaces:

| Descriptor | Size | Format | Geometry |
|---|---:|---:|---:|
| BK stream/loader | `0x220000` | `0x0c001000` | 1024×544 RGBA8 |
| BSF group 0 | `0x320000` | `0x0c001000` | 1024×800 RGBA8 |
| BSF groups 1–3 | `0x300000` | `0x0c001000` | 1024×768 RGBA8 |

These dimensions are caller/profile invariants, not values inferred from PNG.

## 8. Safe editable texture re-encoding

### 8.1 Corruption mechanism corrected by 1.0

The unsafe sequence is:

```text
source GZIP → source GPU bytes → PNG → generic encoder → new GPU bytes → GZIP
```

Even when the PNG is visually unchanged, this can modify engine data:

- BC1/BC2/BC3 encoding is lossy and not a byte inverse of decoding;
- indexed nearest-color selection may choose different P4/P8 indices;
- duplicate palette colors may move to different indices;
- visually identical pixels do not imply identical GPU bytes;
- rebuilding every compressed block discards original GZIP streams, reserved
  words and alignment without an engine requirement.

The defect is in the title-specific serializer before CPK packing, not primarily
in the generic CPK writer.

### 8.2 Extraction contract

Every editable subpackage stores:

- editable PNG;
- exact aggregate post-GZIP destination bytes;
- exact source chunk-table region including block headers, streams and padding;
- FNV-1a fingerprints for both machine-managed artifacts;
- explicit package/descriptor/profile metadata.

The source buffer is physical-layout context and the authoritative no-edit
baseline. It is not used to suppress valid edits.

### 8.3 Build algorithm

`rz-tool` performs:

1. require schema/document version 1;
2. validate archive profile, allocation, header, descriptor, geometry, format,
   stride and chunk-count limits;
3. fingerprint-check both source artifacts;
4. reparse the source chunk table and require JSON metadata, table bytes and
   inflated aggregate buffer to agree;
5. decode source GPU bytes to establish the baseline image;
6. compare the editable PNG with that baseline;
7. encode changed pixels into the original physical layout;
8. rebuild palette/index data when required;
9. recompress only destination chunks whose bytes changed;
10. rebuild or relocate the chunk table when necessary;
11. parse the complete generated subpackage again;
12. inflate all generated blocks and require byte-exact aggregate equality with
    the intended encoded GPU buffer;
13. decode the generated texture and verify format-specific visual invariants;
14. only then pass the payload to the CPK writer.

### 8.4 RGBA and reversible layouts

For reversible layouts, the decoded rebuilt texture must equal the edited PNG
exactly. Physical order, visible dimensions, allocation dimensions and padding
are independently validated.

### 8.5 BC1/BC2/BC3

BC formats remain editable. The safety strategy is block-local, not copy-only:

- unchanged 4×4 storage blocks remain byte-identical;
- changed blocks use source endpoints as optimization seeds;
- candidate endpoints include source values and color extrema;
- local refinement selects a better representable block;
- edge texels outside the visible image preserve source values;
- block bytes are written to the original linear or proven Vita-swizzled
  physical position;
- the new block is immediately decoded and its visual error measured;
- an edit is rejected only when it collapses to no representable change, not
  because BC is inherently lossy.

No arbitrary source-relative quality threshold converts the editor into a
copy-only tool.

### 8.6 P4/P8

Paletted editing rebuilds both indices and palette:

- exact source colors retain their original indices where possible;
- duplicate palette entries remain distinguishable through source index
  history;
- new colors consume available entries deterministically;
- images above 16 or 256 colors use deterministic median-cut quantization;
- P4 writes low nibble before high nibble;
- the unused nibble for an odd visible pixel count retains the source value;
- palette sizes remain exactly `0x40` or `0x400` bytes.

### 8.7 Incremental compressed-table rebuild

Each destination block is compared against the source aggregate buffer:

- unchanged destination bytes reuse the original complete compressed block;
- changed destination bytes produce a new GZIP stream;
- reserved words are retained;
- unchanged table prefix and profile-specific opaque count bits are retained;
- table offsets are regenerated;
- the table remains in place if it fits the original span;
- otherwise the compressed region is relocated with alignment and package
  offsets are updated.

### 8.8 Fail-closed boundaries

Build aborts transactionally on:

- unsupported texture format or unproved layout;
- mip/tile/swizzle combination without a serializer;
- missing or mismatched source artifact fingerprints;
- source table/JSON/decompressed-buffer disagreement;
- overlapping writes or incomplete visible-surface coverage;
- invalid palette size or descriptor mismatch;
- chunk count above 255;
- decompression or reparse mismatch;
- fixed allocation overflow;
- edited lossy block/surface producing no representable visual change.

## 9. Archive-specific image paths

### 9.1 ADDPT

`FUN_8104d652` loads the single ADDPT entry and dispatches its bundle through
`FUN_81053fa6`. The profile uses table delta zero and package-created texture
descriptors.

### 9.2 PT

`FUN_8105403e` and `FUN_81054260` establish PT bundle/package use. PT contains 23
fixed executable allocations. Entries that fail the proven grammar are retained
as explicit opaque payloads instead of being guessed or causing the complete
archive extraction to fail.

### 9.3 BK

`FUN_81053cda` uses table delta `0x1400` and copies the final decompressed data
into an executable-created 1024×544 RGBA8 surface. The editable codec validates
that runtime profile rather than treating each CPK `.bin` entry as a GXT file.

### 9.4 BSF

`FUN_8101d4c8`, `FUN_8101fde8` and `FUN_8101fea6` form the streaming subsystem.
The observed groups use 1024-wide RGBA8 surfaces with heights 800 or 768. BSF
consumes the low byte of the table count word and preserves the remaining bits.

## 10. `sc.cpk`: compiled scene scripts

### 10.1 Ownership and loading

`FUN_81053cb6`:

- accepts IDs below `0x59`;
- selects archive index 1 (`sc.cpk`);
- reads the entry sector count from `0x8111344c`;
- calls the fixed-sector reader.

`FUN_81019b6c` loads the selected entry into the active script buffer and invokes
VM initialization. The ELF contains the VM and metadata tables, not a duplicate
of all script payloads.

### 10.2 Entry layout

All supplied entries conform to:

```text
0x0000..0x007f  voice block
0x0080..0x008f  runtime header
0x0090..         primary u32 offsets
                  align to 0x10
                  secondary records, 7 × u32 each
                  zero padding through 0x1fff
0x2000..end      stream-offset table + compiled payload/data
```

At `entry + 0x80`:

| Relative offset | Type | Meaning |
|---:|---|---|
| `+0x00` | `u32` | primary table offset, canonically `0x10` |
| `+0x04` | `u32` | primary marker count |
| `+0x08` | `u32` | secondary table offset |
| `+0x0c` | `u32` | secondary record count |

The canonical secondary offset is
`align_up(0x10 + primary_count × 4, 0x10)`.

### 10.3 Voice block: `FUN_8101666c`

Entries normally contain a little-endian `u16` voice base followed by zeroes.
The non-voice form begins with `voice_not_exist\0`. The complete `0x80` bytes
are regenerated from this explicit representation.

### 10.4 Stream resolver: `FUN_810003b2`

The routine returns:

```text
script_base + stream_offsets[u16_stream_id]
```

Located direct, conditional, table-select and resume paths pass stream IDs to
this resolver. Offsets may alias and need not be monotonic.

### 10.5 Main interpreter: `FUN_8100132c`

The dispatcher contains 171 explicit control cases in the `0xfefe..0xffff`
range. Directly observed behavior includes:

- words below `0x0e12` on glyph paths;
- `0xfff0` primary dialogue marker;
- `0xfffe` line completion;
- `0xffff` path/script termination;
- `0xff33`, `0xff48`, `0xff49` external image/resource routing.

Unclassified commands and embedded tables remain exact raw `u16` nodes. Stable
1.0 does not assign names or operand widths without evidence.

### 10.6 Dialogue page grammar

The call chain
`FUN_8100132c → FUN_81006a86 → FUN_8104e0a6 → FUN_8104df94` and the complete
89-entry corpus establish:

```text
fff0 marker_index
speaker_glyph*
ffff
page_0_glyph* fffe
[page_1_glyph+ fffe]
[page_2_glyph+ fffe]
```

`FFFE` completes one displayed page; it does not necessarily end the primary
dialogue record. The corpus contains 20,686 primary markers and 37,125 pages:
7,927 one-page dialogues, 9,079 two-page dialogues and 3,680 three-page
dialogues. No observed dialogue has more than three pages. Continuation parsing
accepts only an immediately following glyph-only run terminated by another
`FFFE`. `FFFF`-terminated glyph runs are handled separately: proven secondary,
`FF42`, and `FF8C` grammars become typed text records, while a remaining run of
three or more glyphs causes extraction to fail rather than becoming silent raw
state.

Extraction is fail-closed. The parser records every `FFFE` position in the
compiled word stream and requires a one-to-one match with parsed page endings.
A residual terminator indicates that text-bearing grammar remains outside the
recognized dialogue model, so extraction aborts rather than hiding those words
inside a raw node. The generated relocatable IR is then assembled immediately
and must reproduce the original word stream and secondary relocation records
exactly before an editable project is accepted.

This distinction explains the previously missing opening line. In engine entry
86, marker 1 contains the first page `……レム、レム。`, followed by the pages
`これ、本当にラムたちが` and `言わなくてはいけないの？`. Treating the first
`FFFE` as the end of the whole dialogue left the latter pages in a raw node.

### 10.7 Entry identity and scenario routing

`FUN_81053cb6` accepts an entry ID below `0x59`, indexes the SC sector table with
that same ID and passes it unchanged to the fixed-sector reader. It does not
translate archive position into another scenario number.

The new-game initialization at `0x8101ad94` loads:

```text
r0 = 0x56  scenario entry ID 86
r3 = 0x00  local stream ID 0
bl FUN_8101bec6
```

Therefore engine entry 86 is the main prologue root even though its numeric ID
is not zero. Opcode `FFEF` stores its first operand as the next entry ID and its
second operand as the next local stream ID; the loader later passes that entry
ID to `FUN_81053cb6`. Choices and flags produce a branching directed graph, not
a single chronological file sequence.

The project contract consequently keeps three independent values:

- `entry_id`: engine-visible ITOC ID used unchanged by the VM and loader;
- `order`: CPK iteration/emission order retained for archive reconstruction;
- presentation position: the array position used by `scenario-dialogue.json`
  and the navigation rank recorded by `scenario-routing.json`.

No user-facing script filenames are required for normal extraction.
`scenario-dialogue.json` is the authoritative editing document and lists entry
86 first while preserving `entry_id: 86`. Build joins each edit to the exact
internal dialogue by `(entry_id, marker_index)`, then sorts CPK output by the
preserved archive `order`.

`scenario-routing.json` records the proven root, the order/ID mapping and every
`FFEF` word triple whose target entry and stream are valid. Presentation order is
dependency-aware rather than depth-first. Every observed source is emitted
before its target, and a shared convergence target is delayed until all observed
predecessors have been emitted. Ties use first discovery and source opcode
occurrence order. Disconnected components are appended by engine-ID root order.
This produces a deterministic topological presentation while preserving the
fact that the candidate graph is incomplete and does not define one universal
playthrough chronology.

The physical node order, relocation labels, raw VM words and engine metadata
are machine-managed rebuild state in `.rz-internal/sc-state.json.gz`. Dialogue
Unicode and complete dialogue glyph vectors are not stored there;
`scenario-dialogue.json` is the sole text source. The state bundle stores only
sparse glyph-alias deltas for source IDs that differ from canonical
`charset.json` encoding, plus structural data that cannot be derived from text.
Human-readable per-entry copies are emitted only with `--debug-script-ir`, under
`debug/scenario-ir/`; build does not consume those copies.

### 10.8 Primary marker table

The runtime table contains byte offsets of the ordered marker chain:

```text
fff0 0000
fff0 0001
fff0 0002
...
```

The assembler derives the table after emitting the new payload. It does not copy
stale offsets from extraction.

### 10.9 Secondary records and secondary-target strings

The corpus contains 47 records of seven `u32` words, 329 fields total:

- 119 fields are even in-payload addresses targeting valid boundaries;
- 210 fields are zero or small immediate values;
- no located address splits a primary dialogue span.

All 119 address targets begin with one or more glyph IDs below `0x0e12` and end
with `FFFF`. The target may have additional non-text words after the terminator. Those
words belong to a typed machine suffix; their numeric values are not interpreted
as glyph IDs merely because they are below `0x0e12`. The stable grammar is:

```text
secondary record address
→ glyph* FFFF
→ preserved target suffix
```

These strings include user-visible selection labels such as
`エミリアの質問に真面目に答える`. Treating the target as an opaque raw node
would preserve bytes but hide editable text. The parser therefore emits a
`secondary-string` record in `scenario-dialogue.json`, retains only any sparse
non-canonical glyph-alias positions plus the suffix in machine state, and
resolves the secondary address label after text reassembly.

A concrete boundary case occurs in entry 17, text record 6. The editable text is
`キスといえば口`; its preserved suffix is:

```text
0004 0019 000e ffff
```

The first three values fall inside the glyph-ID numeric range and would decode
to the implausible sequence `．〆¨`, but they are machine operands, not another
string. Consequently, the runtime parser does not classify either standalone raw
words or a typed secondary suffix as text from this numerical pattern alone.
An `FFFE` inside such a suffix remains a hard error because it would overlap the
separately proven dialogue-page grammar. Higher-level semantic names for the
seven fields remain unknown; only the address classification, leading target
string and opaque suffix ownership are claimed.

### 10.10 Editable text and internal relocatable state

The original compiler input is not recoverable: comments, macros, identifier
names and source syntax are absent. The stable project separates the concise
text-editing model from the source-equivalent rebuild model.

Each entry in `scenario-dialogue.json` contains:

- engine `entry_id`;
- `dialogues`, keyed by ordered primary `marker_index`, with editable `speaker`
  and page strings;
- `texts`, keyed by `text_index`, with a fixed grammar tag and editable `text`.

The proven non-dialogue grammar tags are:

- `secondary-string`: glyph string reached through a classified secondary
  payload address;
- `inline-ff42`: glyph string immediately following opcode `FF42`;
- `inline-ff8c`: glyph string immediately following opcode `FF8C`.

The interpreter evidence for the two inline forms is direct. `FUN_81017544`
handles `FF42`, passes `PC + 2` to `FUN_81006b8a`, and advances the VM pointer by
`length * 2 + 4`. `FUN_810174aa`, dispatched by `FF8C`, performs the equivalent
operation through `FUN_81006b0e`. `FUN_8104e182` and `FUN_8104e0c4` scan `short`
values until `-1` (`FFFF`), proving the terminator. Their display buffers accept
at most 21 and 7 glyphs respectively; build rejects edits beyond those limits
rather than allowing engine-side truncation.

The supplied corpus contains:

```text
20,686 primary dialogues
37,125 dialogue pages
119 secondary-string records
1 inline-ff42 record
1 inline-ff8c record
```

The inline strings are `これはコメントです` (`FF42`) and `ぶりっじ`
(`FF8C`) in entry 87. They are extracted because their opcode-width and
terminator behavior are proven; other low-valued operands are not guessed to be
text.

A broad post-classification scan of every raw node finds no `FFFF`-terminated
run of three or more glyph IDs. One two-value run remains in entry 87 and would
decode as `。、` if the values were treated as glyph IDs. Its surrounding words
are:

```text
ffde 0000 3000 0002 0001 ffff
```

This is not text. `FUN_8100b2ac`, the interpreter handler for `FFDE`, calls
`FUN_8100b134` on the operands following the opcode. `FUN_8100b134` interprets
`3000`, `0002`, and `0001` as a variable selector, comparison operator, and
comparison value, with `FFFF` terminating the expression. The raw sequence is
therefore retained as command data.

It is not a general rule that three low-valued words followed by `FFFF` form
text: both command operands and typed secondary suffixes demonstrate the
contrary. Proven anchors (`FFF0`/`FFFE`, secondary relocation targets, `FF42`,
and `FF8C`) and byte-exact reassembly are the authoritative runtime grammar
checks. A numerical scan for additional glyph-range candidates remains in the
offline corpus validator as a review aid, but it does not reject extraction.

`.rz-internal/sc-state.json.gz` is the single machine-managed bundle required to
rebuild. It retains:

- stream labels and physical node order;
- sparse glyph-alias deltas only where a source ID differs from canonical
  `charset.json` encoding;
- exact raw `u16` command/data nodes;
- typed text-node prefix/suffix words;
- symbolic labels for engine-consumed payload addresses;
- secondary relocation records, allocation data and the extracted integrity footer.

It does not retain Unicode dialogue strings or complete source glyph vectors.
Those are reconstructed from `scenario-dialogue.json` and `charset.json` during
build.

For the supplied corpus, the user document contains 599,524 text glyphs. Only
5,995 positions use a non-canonical alias (the alternate long-vowel glyph). The
previous duplicated state bundle was 1,881,611 bytes compressed; the structural
bundle without duplicated text is 224,948 bytes compressed while hydrating back
to the exact original source-node glyph sequences.

Default extraction does not emit per-entry IR/state documents. With
`--debug-script-ir`, diagnostic copies are written to `debug/scenario-ir/`; they
are never build inputs.

Build overlays the user document by `(entry_id, marker_index)` for dialogues and
`(entry_id, text_index)` for non-dialogue strings. Missing, duplicate, unknown
or grammar-mutated records are rejected. Non-dialogue records must remain
non-empty because all three proven grammars contain `glyph+ FFFF`, not an empty
string form. A dialogue assembles as:

```text
fff0 marker_index
speaker glyphs
ffff
page 0 glyphs
fffe
[page 1 glyphs
fffe]
[page 2 glyphs
fffe]
```

A typed non-dialogue string reassembles as its preserved opcode/target prefix,
encoded glyphs, `FFFF`, and preserved suffix. Canonical glyph IDs are generated
from the user text; sparse source aliases are reapplied only when the stored
alias still decodes to the character at that position. This keeps genuine
charset aliases byte-identical without duplicating the full text in machine
state. Variable-length edits and page-count changes are supported because
stream, primary and classified secondary addresses are regenerated.

Extraction fails unless all three engine-anchored coverage conditions hold:

1. every `FFFE` belongs to exactly one parsed dialogue page;
2. no secondary-target string or proven `FF42`/`FF8C` string remains in a raw
   node;
3. the generated source IR reproduces the original payload words and secondary
   relocation records exactly.

A glyph-range run followed by `FFFF` is not a runtime grammar by itself because
VM operands share the same numeric range. Such runs remain an offline audit
signal only.

### 10.11 Allocation tail and integrity footer

The supplied entries follow:

```text
logical payload ending in ffff
zero-filled free capacity
16-byte integrity footer at allocation end
```

The free space is capacity, not source IR. The footer is consumed before VM
initialization. `FUN_81053B7A` calls the function pointer at `DAT_8110C328`
after reading `sector_count << 11` bytes; that pointer is `FUN_8102B4AC`.
The verifier excludes the last 16 bytes, then processes every preceding
16-byte block as two little-endian `u64` lanes:

```text
lane0 = 0x1111111111111111
lane1 = 0x1111111111111111
for each 16-byte block:
    lane0 = wrapping_add(lane0, le_u64(block[0:8]))
    lane1 = wrapping_add(lane1, le_u64(block[8:16]))
footer = le_u64(lane0) || le_u64(lane1)
```

The callback returns zero on mismatch, so the asynchronous archive read never
reaches its successful completion state. This explains why even a same-length
swap such as `これ` to `れこ` hangs when the original footer is preserved.
Build now zero-fills remaining capacity and regenerates both checksum lanes.
Extraction also rejects a source entry whose stored footer does not match.
The algorithm reproduces all 89 original SC footers exactly.

## 11. SC global metadata and ELF patching

### 11.1 Global table at `0x810f9b1c`

Each of the 89 records is:

```text
u16 stream_global_base
u16 stream_count
u16 primary_global_base
u16 primary_count
u16 secondary_global_base
u16 secondary_count
```

`FUN_8101c516`, `FUN_8101c58e`, `FUN_8101c604`, `FUN_8101c6c4` and
`FUN_8101c720` use these bases/counts for global runtime state. Primary base
advances by `ceil(primary_count / 8)`, consistent with bitset storage.

### 11.2 Patch site `0x8111344c`

The SC sector table consists of 89 `(u16 start_sector, u16 sector_count)`
records. `rz-tool` validates the cumulative stock layout and recomputes both
fields from rebuilt entry allocations.

### 11.3 Patch site `0x810f9b1c`

Structural edits may change local stream, primary or secondary counts. Stable
1.0 recomputes every global base/count record from the rebuilt project.

### 11.4 Patch site `0x8101b554`

The stock four bytes encode `movs.w r0, #0x20000`. The patcher accepts only a
fixed documented set of power-of-two encodings through `0x08000000`. It maps
virtual addresses through ELF32 `PT_LOAD` program headers and refuses unknown
signatures, encrypted/non-ELF inputs, or unsupported immediates.

### 11.5 Transaction model

CPK and ELF are written to staging paths. Both are renamed into place only after
all builds, table calculations, signature checks and writes succeed. An error
removes staged outputs rather than leaving a partially updated pair.

### 11.6 Hard limits

The SC patch does not remove all engine constraints:

- the generated runtime header/tables must fit the fixed `0x2000` bytes;
- sector counts and global bases/counts remain `u16`;
- runtime buffer size must match a supported Thumb immediate;
- actual runtime heap availability requires emulator/hardware testing;
- glyph IDs remain bounded to 3,602.

## 12. Charset and `lt.bin`

### 12.1 Renderer geometry

`FUN_8102d78c` selects the 24-pixel path in `FUN_8102d194`. The renderer:

- rejects glyph IDs `>= 0x0e12`;
- computes `glyph_id × 0x120`;
- reads 12 packed bytes per row;
- expands low nibble, then high nibble;
- processes 24 rows.

Therefore:

```text
glyph count       0x0e12 = 3,602
glyph geometry    24 × 24
storage           4 bits/pixel
bytes per glyph   0x120
addressed bytes   0x0e12 × 0x120 = 0x0fd440
file allocation   0x0fd800
tail              0x3c0 bytes
```

### 12.2 Standalone loader and integrity tail

The earlier renderer-only audit was incomplete. The startup state machine at
`0x8101a9de` indexes the standalone table at `0x81129e58`. For resource ID 7 it
loads `0x1fb` sectors into `DAT_811b98e4`, polls completion, then calls the
function pointer at `0x8110c328` as `(destination, byte_size)`. That pointer is
`0x8102b4ad`, selecting Thumb routine `FUN_8102b4ac`. A mismatch prevents the
state machine from advancing.

All located glyph consumers still stop at `0x0fd440`, but the remaining bytes
are split as follows:

```text
0xfd440..0xfd7ef  0x3b0 bytes tail/padding
0xfd7f0..0xfd7ff  0x10 bytes two-lane integrity footer
```

The supplied `lt.bin` has zero-filled tail/padding and footer
`71f70d807e60ce6c9129df8e1a01f723`, reproduced exactly by `FUN_8102b4ac`'s
algorithm. Build may preserve or zero-fill the first `0x3b0` bytes according to
project policy, but it must always regenerate the last `0x10` bytes after the
font atlas has been encoded.

### 12.3 Unicode mapping

`lt.bin` stores bitmaps, not Unicode values. The stable charset is a compacted
JIS X 0208 assigned-character ordering with engine-specific adjustments:

```text
ID 0x0000..0x00cf  assigned JIS ordinal unchanged
ID 0x00d0          engine alias for ー
ID 0x00d1..0x01ea  assigned ordinal +1
ID 0x01eb..0x0e11  assigned ordinal +33
```

The final displacement accounts for the omitted 32-cell box-drawing range plus
the earlier net adjustment. `codec/charset.rs` embeds all 3,602 code points and
the validator checks known corpus positions such as `ス` and `任`.

## 13. `pr.bin`

`FUN_81053670` bounds IDs to 35 and indexes the offset table at `0x81113308`,
proving slot boundaries but not a single grammar.

At least three incompatible consumer families are direct evidence:

1. normal package paths through `FUN_81053788`/`FUN_810538ec`;
2. direct palette/texture slots around IDs `0x0b..0x0e`, where
   `FUN_8100ae84` treats the slot base as palette data and `slot + 0x40` as a
   texture descriptor;
3. compressed atlas slots `0x20..0x22`, where `FUN_8103ef36` uses executable
   offset tables, decompresses a selected record, and copies 50 rows of
   `0x160` bytes into a destination stride of `0x200`.

A universal package parser would corrupt at least two families. `rz-tool`
therefore supports explicit raw extraction/rebuild only. Editable PR requires a
separate proven encoder for every slot family.

## 14. Rebuild safety invariants

The engine consumes package bytes after a fixed sequence of operations: select a
chunk table, inflate each block into a destination buffer at `index * stride`,
install descriptor metadata, copy texture storage, and then copy palette storage
when present. The rebuild pipeline treats this engine-observed post-inflate GPU
buffer as the authoritative target, not the PNG alone and not the packed GZIP
stream alone.

The implemented image rebuild contract follows these invariants:

1. the package profile, descriptor fields, table shape and source fingerprints
   are checked before any write;
2. edited PNG pixels are encoded into the same physical GPU layout proven by the
   descriptor and loader path;
3. BC1/BC2/BC3 blocks that are visually unchanged retain their original block
   bytes, while changed blocks are encoded with source endpoints as seeds and
   edge texels outside the visible rectangle preserved;
4. P4/P8 textures rebuild palette and index data together, preserving stable
   source indices when possible and using deterministic quantization only when
   the edited image exceeds the native palette capacity;
5. only destination chunks whose bytes changed are recompressed;
6. the rebuilt package is parsed and inflated again, and the resulting aggregate
   GPU buffer must exactly match the intended encoded buffer before CPK packing;
7. lossy texture formats are editable by default, but an edit must be
   representable in the target format and must survive the package-level
   verification pass;
8. unproven package or texture layouts fail before archive writing rather than
   falling back to an opaque raw payload in editable mode.

Script rebuild follows the same principle: the assembler regenerates the engine
regions that are structurally understood, preserves machine-managed metadata in
the companion document, reparses the generated entry, and requires matching
allocation and relocation invariants before the rebuilt `sc.cpk` is emitted.

`lt.bin` rebuild is constrained by the renderer's proven addressing formula:
3,602 glyphs, `24 x 24` pixels, 4 bits per pixel, `0x120` bytes per glyph. The
addressed glyph region is rebuilt from the atlas and the non-rendered tail is
kept under explicit policy rather than inferred as hidden texture data.

`pr.bin` remains outside editable image rebuild because the executable proves
slot boundaries but also proves incompatible consumer grammars. Treating all PR
slots as one package family would violate the safety rule above.

## 15. Primary routine index

| Routine/address | Role |
|---|---|
| `FUN_810224bc` | resource registry installation |
| `FUN_8101b504` | major resource/runtime allocations |
| `FUN_81053b7a` | fixed-sector archive entry reader |
| `FUN_81066e32` | ITOC subtable search |
| `FUN_81066ed2` | merged ITOC prefix-size calculation |
| `FUN_8106706c` | combined DataL/DataH lookup |
| `FUN_81053694` | shared compressed image-package loader |
| `FUN_81034e4a` | GZIP block wrapper |
| `FUN_810225fa` | supplemental `0x20`-byte record installer |
| `FUN_81053fa6` | ADDPT/PT bundle loader |
| `FUN_81053cda` | BK package/runtime surface path |
| `FUN_8101d4c8` / `FUN_8101fde8` / `FUN_8101fea6` | BSF streaming path |
| `FUN_8102fdca` | texture byte copy |
| `FUN_8102fd82` | palette byte copy |
| `FUN_81053cb6` | SC entry load |
| `FUN_81019b6c` | SC reload and VM initialization |
| `FUN_8101a6ea` / `FUN_81000512` | SC entry/runtime layout initialization |
| `FUN_8101666c` | voice block initialization |
| `FUN_810003b2` | stream-ID resolver |
| `FUN_8100132c` | main script interpreter |
| `FUN_81006a86` / `FUN_8104e0a6` / `FUN_8104df94` | dialogue parser path |
| `FUN_8101c516` family | SC global base/count consumers |
| `FUN_8102d78c` / `FUN_8102d194` | 24-pixel LT glyph renderer |
| `FUN_81053670` | PR slot lookup |
| `FUN_8100ae84` | PR direct palette/texture path |
| `FUN_8103ef36` | PR compressed atlas path |

## 11. Common fixed-sector integrity callback

The earlier analysis incorrectly scoped `FUN_8102B4AC` first to scenario
entries and then only to CPK entries. `FUN_81053B7A` is the shared fixed-sector
reader used by BK, BSF, PT, ADDPT and SC callers. The standalone LT/PR/SE state
machine at `0x8101A9DE` invokes the same callback after its own asynchronous
reads. The CPK reader's completed-read branch loads the callback from
`0x8110C2F8 + 0x30 = 0x8110C328` and invokes it as `(buffer, sectors << 11)`.
The executable stores `0x8102B4AD` at that address, selecting Thumb routine
`FUN_8102B4AC`.

Relevant instructions:

```text
FUN_81053B7A
81053c7c  ldr   r2, [r0, #0x30]
81053c7e  lsls  r1, r6, #0xb
81053c80  adds  r0, r7, #0
81053c82  blx   r2
81053c84  cmp   r0, #1

FUN_8102B4AC
8102b4b2  add.w ip, r1, r0
8102b4b6  ldrd  r6, r7, [ip, #-0x10]
8102b4be  ldrd  r8, sb, [ip, #-0x8]
8102b4c6  subs  r1, #0x10
8102b4d0  ldm.w r0, {sl, fp}
8102b4d6  adds.w r2, r2, sl
8102b4da  adc.w r3, r3, fp
8102b4de  ldrd  sl, fp, [r0, #8]
8102b4e2  adds.w r4, r4, sl
8102b4ea  adc.w r5, r5, fp
8102b4f2..8102b500 compare both calculated u64 lanes with the final 16 bytes
```


The standalone loader performs the equivalent verification after loading LT:

```text
8101abdc  movw  r1, #0x9e58
8101abe0  movt  r1, #0x8112       ; standalone table
8101abe6  ldr   r2, [r0, r1]      ; resource ID
8101abea  ldr   r3, [r0, #8]      ; sector count
8101abf0  ldr   r4, [r0, #0xc]    ; destination
8101ac00  blx   r5                ; begin asynchronous read
...
8101ac54  movw  r1, #0x9e58
8101ac60  ldr   r3, [r0, #4]      ; exact byte size
8101ac66  ldr   r0, [r0, #0xc]    ; destination
8101ac6c  ldr   r2, [r2, #0x30]   ; 0x8110c328
8101ac70  blx   r2                ; FUN_8102b4ac(buffer, size)
8101ac72  cmp   r0, #2
```

For LT, the table record is resource ID 7, size `0xfd800`, sectors `0x1fb`.

`FUN_81053CDA` proves BK uses that reader before package decompression:

```text
81053da0  adds  r1, r5, #0
81053da2  bl    0x81021034       ; executable sector count
81053da6  adds  r2, r0, #0
81053da8  adds  r1, r6, #0      ; destination allocation
81053dac  movs  r0, #4          ; bk.cpk registry index
81053dae  bl    0x81053b7a
81053db2  cmp   r0, #0
81053db4  beq   load failure
```

The checksum is therefore a fixed-allocation resource invariant, not image
metadata, not an SC-only footer, and not limited to CPK containers. On the
supplied `bk.cpk`:

```text
stock entries valid       326/326
no-edit rebuilt valid     326/326
one-pixel old-tool build  325/326 (entry 325 stale)
footer-regenerated build  326/326
```

For entry 325, the original footer is
`c61022978231856927f537a2f1992222`; after changing pixel `(512,272)` from
`FFFFFFFF` to `00FFFFFF`, the required footer becomes
`53185a780ff7c24255f913de1533f7ef`. The old image serializer preserved the
former value and the common reader rejected the entry before GZIP processing.

The corrected image encoder writes the footer only after output is padded to
the exact executable allocation. This also prevents a future package relocation
or recompression path from calculating the checksum over an intermediate size.
