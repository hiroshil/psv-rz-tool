# Standard Game Localization Workflow (RZ Project - PCSG00933)

This document defines the standard workflow for Vietnamese localization using `rz-tool` and `vwf_patcher` under the current ownership model of the two tools.

## General Principles

- `font.tbl` is the `glyph_id -> Unicode` mapping source used consistently across LT/VWF/SC.
- `lt_build_helper.py` generates `lt-project/font-widths.json` from the exact font metrics used to rasterize each glyph. This generated width artifact should be shared by runtime VWF and build-time wrapping. `font.cnf` is only an optional override/tuning layer.
- `vwf_patcher` owns the VWF hooks, width table, speaker VWF, and runtime glyph limit.
- `rz-tool build lt` owns the allocation/load-size/sector-count values for `lt.bin`.
- `rz-tool build sc` owns the allocation/metadata/script-buffer sizing for `sc.cpk`.
- `extract-lt-alloc` and `extract-sc-alloc` are for **re-extracting assets that have already been built/patched**, not mandatory steps when extracting stock assets for the first time.
- The EBOOT output from each step must be used as the EBOOT input for the next step. Do not switch back to an older EBOOT in the middle of the pipeline.

## Phase 1: Font and VWF Processing (Variable Width Font)

Because the original Japanese game font does not contain all Vietnamese characters, the LT project must be expanded, the VWF runtime must be patched, and then `rz-tool` must update the `lt.bin` allocation.

### Step 1.1 - Extract the Font

#### Starting from stock `lt.bin`

No Allocation Map is required:

```bash
rz-tool extract lt.bin lt-project
```

#### Re-extracting an already modified/built `lt.bin`

You must extract the Allocation Map from the exact EBOOT that belongs to that `lt.bin`:

```bash
rz-tool extract-lt-alloc eboot_patched_lt.elf lt-allocation.json
rz-tool extract lt.bin lt-project --allocation-map lt-allocation.json
```

Do not use `extract-lt-alloc` on an EBOOT that has only been processed by `vwf_patcher` but has not yet gone through `rz-tool build lt`: the runtime glyph limit may already have been increased while the LT allocation is still stock, which would make the map invalid.

### Step 1.2 - Expand/Update the LT Project

Use the helper to update the project directly:

- `lt-project/lt-atlas.png`
- `lt-project/lt-font.json.glyph_count`

```bash
python psv-rz-vwf-patcher/tools/lt_build_helper.py \
  --i lt-project
```

The tool does not build `lt.bin`.

Under the current contract:

- `font.tbl` is auto-resolved and acts as the mapping/profile source;
- `font-widths.json` is generated automatically from the selected font horizontal advance for each glyph;
- `font.cnf` is optional override/tuning only; explicit widths override metrics and `[default]` is fallback-only for preserved glyphs that cannot be measured;
- `Arial.ttf` is auto-resolved as the default font;
- if multiple fonts are needed, pass `--font-file` multiple times in priority order.

Example:

```bash
python psv-rz-vwf-patcher/tools/lt_build_helper.py \
  --i lt-project \
  --font-file Arial.ttf \
  --font-file fallback.ttf
```

If you want to edit glyphs manually, run the helper first to expand the atlas/profile, then edit `lt-atlas.png` directly. Do not run the helper again after manual edits unless you intentionally want the mapped glyph cells to be rasterized again.

### Step 1.3 - Patch EBOOT for VWF Support

Patch from the stock EBOOT:

```bash
python psv-rz-vwf-patcher/rz_vwf_stock_patcher.py \
  --eboot-in eboot.bin.elf.org \
  --eboot-out vwf_patched_eboot.bin.elf \
  --font-tbl psv-rz-vwf-patcher/examples/font.tbl \
  --width-table lt-project/font-widths.json \
  --speaker-max-width 192
```

At this step, `vwf_patcher` is responsible for:

- VWF renderer/hooks;
- the runtime width table;
- speaker VWF and speaker pixel limit;
- the runtime glyph limit.

`vwf_patcher` **no longer increases the allocation/load-size/sector-count values for `lt.bin`**.

### Step 1.4 - Build `lt.bin` and Update LT Allocation in EBOOT

```bash
rz-tool build lt-project lt.bin \
  --eboot-in vwf_patched_eboot.bin.elf \
  --eboot-out eboot_patched_lt.elf
```

`rz-tool` will:

1. build `lt.bin`;
2. verify that `lt-font.json.glyph_count` matches the runtime glyph limit in EBOOT;
3. compute the required allocation from the newly built `lt.bin`;
4. update the LT allocation/load-size/sector-count values in the output EBOOT.

For the first LT build from the correct `vwf_patched_eboot.bin.elf`, `-f` is not required.

If the input EBOOT has already been modified by another LT/SC allocation build inside the protected runtime hash range, use:

```bash
rz-tool build lt-project lt.bin \
  --eboot-in previous_patched_eboot.elf \
  --eboot-out eboot_patched_lt.elf \
  -f
```

`-f` only bypasses a `VWF_RUNTIME_HASH_RANGE_SHA256` mismatch; it does not bypass `glyph_count`, allocation, alignment, or ELF validation.

## Phase 2: Scenario Extraction and Translation (`sc.cpk`)

The main in-game text data is stored in `sc.cpk`.

### Step 2.1 - Extract the Scenario

#### Starting from stock `sc.cpk`

No SC Allocation Map is required:

```bash
rz-tool extract sc.cpk sc-project \
  --charset-map font.tbl
```

#### Re-extracting an already built `sc.cpk`

Extract the Allocation Map from the exact EBOOT corresponding to that `sc.cpk`:

```bash
rz-tool extract-sc-alloc patched_eboot.elf sc-allocation.json

rz-tool extract sc.cpk sc-project \
  --charset-map font.tbl \
  --allocation-map sc-allocation.json
```

### Step 2.2 - Translate

Edit:

```text
sc-project/scenario-dialogue.json
```

Focus on:

- `text`: displayed dialogue/content;
- `speaker`: character names.

The translation should be split into small batches to make JSON structure, markers, and wrapping easier to validate.

## Phase 3: Build the Scenario and Final EBOOT

After translation is complete:

```bash
rz-tool build sc-project sc_patched.cpk \
  --eboot-in eboot_patched_lt.elf \
  --eboot-out final_eboot.bin.elf \
  --charset-map font.tbl \
  --wrap-width-table lt-project/font-widths.json \
  --wrap-mode word \
  --wrap-rows 3 \
  -f
```

In the standard workflow described here, `-f` is intentional because `eboot_patched_lt.elf` has already had its LT allocation modified by `rz-tool build lt` inside the protected runtime hash range.

`-f` **does not** mean bypassing all checks; it only allows the process to continue when `VWF_RUNTIME_HASH_RANGE_SHA256` no longer matches the strict hash of the standalone VWF-patched EBOOT.

If the SC build is run directly on an EBOOT produced only by standalone `vwf_patcher`, and no allocation build has modified it yet, `-f` may be omitted.

### Dialogue Metadata

If wrapping creates continuation/marker metadata, `rz-tool` may generate:

```text
sc_patched.cpk.rz-dialogue-meta.json
```

Keep this file next to `sc_patched.cpk`. It is used to restore/fold back dialogue continuation data correctly when re-extracting an already built archive.

## Phase 4: Package EBOOT as FSELF for PS Vita

The current input is an ELF, so create the final FSELF:

```bash
vita-make-fself.exe -c final_eboot.bin.elf eboot.bin
```

`eboot.bin` is the file to deploy to the PS Vita.

Do not use an older EBOOT ELF in place of `final_eboot.bin.elf`, because the final EBOOT must contain all of the following:

- the VWF runtime patch;
- the LT runtime glyph limit;
- the current LT allocation;
- the current SC allocation/metadata/script-buffer sizing.

## Phase 5: Validation and Re-extract Check

### Step 5.1 - Validator

```bash
python tools/validate_engine_assets.py \
  --elf final_eboot.bin.elf \
  --secrect secrect.json
```

`--secrect` is the current spelling used by the validator CLI.

### Step 5.2 - Re-check Allocation Maps from the Final EBOOT

```bash
rz-tool extract-lt-alloc final_eboot.bin.elf lt-allocation.final.json
rz-tool extract-sc-alloc final_eboot.bin.elf sc-allocation.final.json
```

These two maps should be stored with the final build because they describe the actual allocation/runtime profile of the deployed EBOOT.

### Step 5.3 - Recommended Re-extract Test

To verify round-trip behavior after the build:

```bash
rz-tool extract lt.bin lt-reextract \
  --allocation-map lt-allocation.final.json
```

and:

```bash
rz-tool extract sc_patched.cpk sc-reextract \
  --charset-map font.tbl \
  --allocation-map sc-allocation.final.json
```

If the following file exists:

```text
sc_patched.cpk.rz-dialogue-meta.json
```

keep it next to the archive so `rz-tool` can restore continuation dialogue according to the correct logic.

## Standard Pipeline Summary

```text
stock lt.bin
    |
    v
rz-tool extract
    |
    v
LT project
    |
    v
lt_build_helper.py --i
    |
    v
lt-atlas.png + lt-font.json.glyph_count

stock eboot.bin.elf
    |
    v
vwf_patcher
    |
    v
VWF-patched EBOOT
    |
    +---------------------+
    |                     |
    v                     |
rz-tool build LT          |
    |                     |
    v                     |
lt.bin + LT-patched EBOOT |
    |                     |
    +----------+----------+
               |
               v
        rz-tool build SC -f
               |
               v
   sc_patched.cpk + final ELF
               |
               v
       vita-make-fself
               |
               v
          deploy/test
```

## Important Rules

1. Use the same `font.tbl` for LT/VWF/SC charset mapping.
2. Use the same `lt-project/font-widths.json` for runtime VWF and SC wrapping; use `font.cnf` only as optional override/tuning.
3. `vwf_patcher` does not own LT allocation.
4. `rz-tool build lt` must always receive `--eboot-in` and `--eboot-out`.
5. After each step that outputs an EBOOT, use that EBOOT output as the input for the next step.
6. Use `-f` only to intentionally bypass runtime hash mismatch when chaining allocation builds.
7. The Allocation Map must be extracted from the EBOOT corresponding to the exact asset that was built/deployed.
8. `extract-lt-alloc` and `extract-sc-alloc` are re-extract tools, not mandatory steps when starting from stock assets.
