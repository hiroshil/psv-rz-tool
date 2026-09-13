# Stable runtime validation - mixed stock/custom VWF cursor

## Status

`PASS` for both runtime deploy probes reported by tester:

- `deploy_A_cursor_single`: `ラム` no longer appears blank and is no longer pushed to the end of the row.
- `deploy_B_continuation`: no visible error was observed in the long continuation/wrap/fold-back test.

## Interpretation

The tested result validates the E37E mixed-cursor model used by the stable source package:

- the root cause was the old helper mixing two cursor models on one row;
- custom/VWF glyphs used the pixel cursor at `render_state+0x12`;
- stock glyphs routed back to stock continuation used a fixed-cell X model;
- after narrow custom glyphs, stock Japanese glyphs such as `ラム` could render far to the right instead of at the expected visual position.

## Stable helper contract

The E37E helper now handles every glyph below `glyph_range_end_exclusive` through the same pixel-cursor rendering path:

```c
if (glyph >= glyph_range_end_exclusive) {
    goto stock_continuation;
}

render_glyph_bitmap(glyph);
blit_using_pixel_cursor();

if (glyph < 0x0E12) {
    advance = 24;
} else {
    advance = width_table[glyph - 0x0E12];
}

pixel_cursor += advance;
fixed_cell_counter += 1;
update_cursor_state();
return 1;
```

This preserves stock glyph visual width while avoiding a cursor-model split when stock glyphs follow VWF custom glyphs.

## What this stable update does not change

- It does not move SC allocation ownership into the VWF patcher.
- It does not change the bucketed LT/runtime capacity formula.
- It does not change `font.tbl` or `font.cnf`.
- It does not claim the supplied `rz-tool-step102` can rebuild expanded `lt.bin`; that remains a separate rz-tool LT build-path issue.

## Deployment implication

The stable patcher source is this package. The renderer contract is documented in `README.md`. Runtime deploy bundles should use an EBOOT produced by this patcher, the known-good expanded `lt.bin`, and `sc.cpk` built by `rz-tool` with matching `font.tbl`, `font.cnf`, and sidecar metadata.
