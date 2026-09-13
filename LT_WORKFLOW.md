# LT workflow contract

The VWF patcher owns EBOOT runtime changes only. It does not own final `lt.bin` packing.

`tools/build_dynamic_lt_from_source.py` prepares an editable LT project that was created by `rz-tool extract lt.bin ...`:

1. copy the stock rz-tool LT project;
2. read `font.tbl` to derive the custom glyph range and bucketed runtime glyph count;
3. copy existing custom glyph bitmap cells from a supplied glyph-source LT into `lt-atlas.png`;
4. update `lt-font.json` so the project describes the expanded glyph capacity.

The final deployed `lt.bin` should be produced by:

```bash
rz-tool build lt_project_vwf lt.bin
```

The helper has optional `--rz-tool` and `--out-lt` arguments only to chain that final command. If the supplied rz-tool rejects expanded LT geometry, the failure belongs to the rz-tool LT build path, not to EBOOT patching.

This separation matters because the same source glyph atlas may be valid while the final binary packer still needs rz-tool-side support for expanded VWF LT geometry.
