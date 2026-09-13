# Runtime profile formula proof

## Result

The production profile is now derived from `font.tbl` by a bucketed capacity formula, not by a manual `--runtime-glyph-limit` and not by a blind hard-coded `0x0F40`.

```text
glyph_range_end_exclusive = max_custom_glyph_id + 1
glyph_runtime_limit       = align_up(glyph_range_end_exclusive, 0x40) + 0x40
lt_size                   = align_up(glyph_runtime_limit * 0x120 + 0x3C0, 0x800)
```

For the current `font.tbl`:

```text
used_end                  = 0x0EF7
align_up(used_end, 0x40) = 0x0F00
runtime limit             = 0x0F40
LT size                   = 0x113000
LT sectors                = 0x0226
```

## Evidence

1. Stock engine evidence gives the byte-size terms: `0x0E12 * 0x120 + 0x3C0 = 0xFD800`, matching stock `lt.bin`.
2. `rz-tool` uses 64 columns for LT atlas authoring (`DEFAULT_COLUMNS = 64`), which gives the `0x40` glyph bucket.
3. The exact-fit profile `0x0EF7 / 0x10E000` failed runtime.
4. The recovered internal-good profile `0x0F40 / 0x113000 / 0x0226` booted and equals `align_up(0x0EF7, 0x40) + 0x40`.
5. The guard bucket does not expand VWF coverage: the helper still uses `glyph_range_end_exclusive = 0x0EF7` and the width table still has 229 bytes.

## Validation

- The stable patcher output EBOOT must match the validated patch-only output SHA `6597b624915fb45cb1e34b4a9d73a0748452362b5e6e374c75de7b34c7b75be3` for the bundled font table.
- The LT helper output must match the recovered runtime-good LT size/profile for the bundled font table.
- SC allocation remains out of patcher scope. Deploy B may use an additional rz-tool SC allocation EBOOT patch after the patcher output.

## Claim boundary

No stock ASM line literally states `+0x40`. The rule is accepted as production because it is the smallest deterministic formula that reproduces the recovered runtime-good profile while preserving all engine-proven size terms and avoiding the runtime-rejected exact-fit profile.

## Stable cursor validation

The formula proof covers capacity and LT size. The stable package additionally records runtime validation for the E37E mixed stock/custom cursor fix: Deploy A passed the direct `custom + ラム` case, and Deploy B passed the continuation/wrap/fold-back case.
