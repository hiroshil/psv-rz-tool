# ENGINE_ANALYSIS - Keystone VWF patcher, v1 stable

## Scope

This patcher is not a blind byte-diff patcher. It patches the stock RZ Vita `eboot.bin.elf` by applying named engine patches whose sites are supported by disassembly from `rz.zip/output.asm` and Capstone/Keystone verification.

Stable v1 uses three ownership rules:

1. SC allocation is not patched by the VWF patcher. `rz-tool build ... --eboot-in/--eboot-out` owns SC allocation when a rebuilt `sc.cpk` actually requires it.
2. The visible custom glyph range is derived from `font.tbl`; the engine runtime capacity is derived from that range by a 0x40-glyph bucket plus one guard bucket.
3. The width table is compiled from the same `font.cnf`/JSON file passed to `rz-tool --wrap-width-table`.

Address mapping used by the patcher:

```text
code PT_LOAD: VA 0x81000000 -> file offset 0x000000E0
data PT_LOAD: VA 0x81128000 -> file offset 0x00127FE0
```

## Derived profile contract

The engine-proven stock glyph/control split is `0x0E12`. The patcher treats `0x0E12` as the start of the expanded glyph bank because the VM originally considers words below this value glyph IDs and words at or above it control opcodes.

The production profile distinguishes **visible custom coverage** from **engine runtime capacity**:

```text
glyph_range_start         = 0x0E12
glyph_range_end_exclusive = max(glyph_id >= 0x0E12 in font.tbl) + 1
glyph_runtime_limit       = align_up(glyph_range_end_exclusive, 0x40) + 0x40
lt_size                   = align_up(glyph_runtime_limit * 0x120 + 0x3C0, 0x800)
lt_sector_count           = lt_size / 0x800
width_table_length        = glyph_range_end_exclusive - 0x0E12
```

The width table and VWF helper range stop at `glyph_range_end_exclusive`; guard slots are not treated as custom VWF glyphs unless future `font.tbl` entries map them. The runtime limit is patched only into engine glyph acceptance / LT capacity sites.

For the bundled `examples/font.tbl`, the profile is:

```text
glyph_range_start         = 0x0E12
glyph_range_end_exclusive = 0x0EF7
align_up(0x0EF7, 0x40)   = 0x0F00
glyph_runtime_limit       = 0x0F40
custom glyph count        = 229
lt_size                   = 0x113000
lt_sector_count           = 0x0226
```

### Evidence for the 0x40 bucket + one guard bucket

This formula is not a cosmetic slack value. It is the smallest rule currently supported by both static structure and runtime A/B evidence:

1. **Engine glyph bytes are linear 24x24 4bpp records.** `FUN_8101B504` allocates stock `lt.bin` as `0xFD800`, and the renderer consumes `glyph_id * 0x120` bytes. Stock `0x0E12 * 0x120 + 0x3C0 = 0xFD800`, proving the `0x120` glyph bytes and `0x3C0` tail terms.
2. **The editing/tooling representation uses 64 glyphs per atlas row.** `rz-tool` LT decode/encode uses `DEFAULT_COLUMNS = 64`. Custom glyph additions are authored as rows in that 64-column atlas model even though the binary is linearized.
3. **Exact used-end capacity was runtime-rejected.** For the current font table, `max_custom + 1 = 0x0EF7`. The exact formula produced `glyph_runtime_limit = 0x0EF7` and `lt_size = 0x10E000`; that EBOOT/LT profile failed runtime.
4. **The recovered internal-good profile is exactly bucket+guard.** The verified/internal profile uses `glyph_runtime_limit = 0x0F40`, `lt_size = 0x113000`, `lt_sector_count = 0x0226`. This equals `align_up(0x0EF7, 0x40) + 0x40`. Replacing only the dynamic-profile EBOOT with this profile made the same `sc.cpk` boot.
5. **The guard bucket does not expand user-visible coverage.** The VWF helper still checks `glyph < 0x0EF7` before indexing the 229-byte width table. Therefore `0x0EF7..0x0F3F` are capacity guard slots, not hidden characters.

Claim boundary: no located stock ASM instruction literally says `+0x40 guard bucket`. The formula is production-acceptable because it is the minimal deterministic rule that (a) preserves every engine-proven byte-size term, (b) follows the 64-column LT authoring invariant, (c) reproduces the recovered working internal profile, and (d) avoids the runtime-rejected exact-fit profile.

## Patch inventory

| Patch | Site | Engine evidence | Patch behavior |
|---|---:|---|---|
| VM glyph limit | `0x8100140E` | Main scenario interpreter treats words `< 0x0E12` as glyphs and `>= 0x0E12` as control path | Raise upper bound to derived `glyph_runtime_limit` |
| Aux glyph limit | `0x8102D37C` | Large switch path uses the same `0x0E12` glyph/control split | Raise upper bound to derived `glyph_runtime_limit` |
| String-raster glyph limit | `0x8104DC38` | String raster scan compares each word with `0x0E12` before glyph comparison | Raise upper bound to derived `glyph_runtime_limit` |
| Expanded LT allocation | `0x8101B510`, `0x81129E5C`, `0x81129E60` | LT setup allocates `0x000FD800` and records file size/sector count in a data table | Write derived `lt_size` and `lt_sector_count` |
| VWF glyph renderer hook | `0x8104E37E -> 0x8110B036` | Stock renderer draws a glyph then advances cursor by fixed `0x18` pixels | Render every glyph below `glyph_range_end_exclusive` through one pixel-cursor path; stock glyphs advance `0x18`, custom glyphs advance by width table |
| Line/wrap guard hook | `0x8104E24C -> 0x8110B150` | Stock line update copies column limit `r2` into `r8` and compares against current column | Preserve calibrated calibrated guard extension `+0x6A` |
| Width table | `0x8112B070` | VWF helper reads `u8 width[glyph - 0x0E12]` from this VA | Compile from `font.tbl` + supplied font.cnf width config |

## 1. Main VM glyph limit at `0x8100140E`

### Disassembly evidence

```asm
8100140e: movw r0, #0xe12
81001412: cmp  lr, r0
81001414: bge  0x8100141a
81001416: adds r6, r4, #0x0
81001418: b    0x81001436
```

### Pseudo-C

```c
word = current_u16;
if (word < 0x0E12) {
    is_glyph = true;
} else {
    goto control_opcode_path;
}
```

### Patch rationale

Expanded glyph IDs start at `0x0E12`. Leaving this comparison unchanged makes every expanded glyph enter the control-opcode path instead of the glyph path. The patcher preserves the same control-flow shape and changes only the immediate bound to the bucketed `glyph_runtime_limit`.

## 2. Auxiliary glyph limit at `0x8102D37C`

### Disassembly evidence

```asm
8102d37c: movw r1, #0xe12
8102d380: cmp  r4, r1
8102d382: bge.w 0x8102d788
```

### Pseudo-C

```c
if (glyph_or_word >= 0x0E12) {
    goto non_glyph_or_error_case;
}
```

### Patch rationale

This secondary path must accept the same expanded glyph domain/capacity as the main VM path. It is patched to the same bucketed `glyph_runtime_limit`.

## 3. String-raster glyph limit at `0x8104DC38`

### Disassembly evidence

```asm
8104dc38: movw r0, #0xe12
8104dc3c: ldrh r1, [r2,#0x0]
8104dc3e: cmp  r1, r0
8104dc40: bge  0x8104dc54
8104dc42: ldrh r0, [r7,#0x0]
8104dc44: cmp  r1, r0
```

### Pseudo-C

```c
for (i = 0; i < count; i++) {
    word = *scan++;
    if (word >= 0x0E12) break;
    if (word == target_glyph) found = true;
}
```

### Patch rationale

Without raising this bound, custom glyphs terminate the scan as control words. The patch raises it to the same bucketed `glyph_runtime_limit`.

## 4. Expanded `lt.bin` allocation and tables

### Disassembly evidence

```asm
FUN_8101b504:
8101b50e: ldr  r2, [r4,#0x4]
8101b510: movs.w r0, #0xd800
8101b514: movt r0, #0xf
8101b518: movs r1, #0x0
8101b51a: blx  r2
8101b526: str  r0, [r1,#0x0]
```

The immediate pair loads `r0 = 0x000FD800`, matching the stock `lt.bin` size. The stock data table also contains:

```text
0x81129E5C = 0x000FD800
0x81129E60 = 0x000001FB
```

### Pseudo-C

```c
lt_buffer = alloc(lt_size, 0);
DAT_811b98e4 = lt_buffer;
lt_size_table = lt_size;
lt_sector_count_table = lt_size / 0x800;
```

### Patch rationale

The renderer indexes glyph pixels in 24x24 4bpp tiles. One glyph consumes `0x120` bytes. Stock `lt.bin` has a `0x3C0` tail and the loader/table values are sector-aligned. Therefore stable v1 derives:

```text
lt_size = align_up(glyph_runtime_limit * 0x120 + 0x3C0, 0x800)
sector_count = lt_size / 0x800
```

For the current `font.tbl`, the bucketed formula derives the recovered working `0x113000/0x226` pair without exposing a manual override flag.

## 5. Stock renderer `FUN_8104E37E` and mixed stock/VWF hook

### Disassembly evidence

The function at `0x8104E37E` is the glyph renderer/advance path. The runtime hook replaces its entry with a branch to an executable cave at `0x8110B036`. The stable mixed-cursor update keeps the validated correction: the helper uses one pixel-cursor model for every glyph below `glyph_range_end_exclusive`, not just for custom glyphs.

Current helper behavior:

```c
if (glyph >= glyph_range_end_exclusive) {
    goto stock_FUN_8104E37E_continuation;
}

render_glyph_bitmap(glyph);
blit_to_text_surface(..., pixel_cursor_x, ...);

if (glyph < glyph_range_start) {
    advance = 0x18;              // stock glyph, same visual width as original engine
} else {
    advance = width_table[glyph - glyph_range_start];
}

pixel_cursor_x += advance;
fixed_cell_counter += 1;
update_cursor_state();
return 1;
```

### Patch rationale

Older VWF helpers routed stock glyphs such as `ラム` back to the original stock continuation. That mixed two X-coordinate models on the same row: custom glyphs used the VWF pixel cursor at `render_state+0x12`, while stock glyphs used the fixed-cell counter (`render_state[0] * 24`). After narrow custom glyphs, the two positions diverged, making stock glyphs appear blank at the expected position or appear later near the row end.

The stable helper keeps stock-only text visually stable by advancing stock glyphs below `0x0E12` by `0x18`, but it renders those stock glyphs through the same pixel-cursor blit path as custom VWF glyphs. Runtime validation confirmed the mixed `custom + ラム` case no longer fails in Deploy A and continued to pass in the long continuation/wrap/fold-back Deploy B.

## 6. Width table at `0x8112B070`

### Contract

```c
uint8_t advance = width_table[glyph_id - 0x0E12];
```

Stable v1 compiles this table from the same compact advance-group `font.cnf` used by `rz-tool build --wrap-width-table`, so SC word wrapping and runtime glyph advance use the same width model.

## 7. Line/wrap guard hook at `0x8104E24C`

### Disassembly evidence

```asm
8104e24c: uxtb.w r8, r2
```

This value feeds the stock line/cursor guard logic. The calibrated runtime patch redirects this instruction to a helper:

```asm
uxtb.w r8, r2
add.w  r8, r8, #0x6a
b.w    0x8104e250
```

### Patch rationale

The guard extension is retained because it is part of the previously verified runtime VWF profile. It is independent from SC allocation and independent from the number of custom glyphs.

## 8. SC allocation ownership

Scenario loading uses the executable table at `0x8111344C`:

```asm
81053ccc: add.w r0, r0, r3, lsl #2
81053cd0: ldrh r2, [r0, #2] ; sector_count
81053cd2: movs r0, #1       ; sc.cpk
81053cd4: bl 0x81053b7a
```

Pseudo-C:

```c
sector_count = sc_allocation_table[entry_id].sector_count;
load_cpk_entry(1, destination, sector_count << 11);
```

This is real engine evidence, but it is not VWF patcher ownership. Stable v1 deliberately does not patch this table. `rz-tool` already has `extract-alloc` and `build --eboot-in/--eboot-out`; when a rebuilt `sc.cpk` needs allocation changes, `rz-tool` is the component that should update the table.

## Validation rules

The patcher must:

- reject non-stock input ELF by SHA-256;
- guard every stock instruction overwrite by old bytes;
- assemble hook code with Keystone;
- derive glyph/LT capacity from `font.tbl` using `align_up(glyph_range_end_exclusive, 0x40) + 0x40`;
- compile width bytes from the supplied font.cnf compact advance-group width config or JSON file;
- not expose or require any SC allocation map;
- emit a JSON summary containing derived profile, width hash and changed ranges.
