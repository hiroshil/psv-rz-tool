# Evidence

This directory is part of the stable source package. These JSON files preserve the recovered runtime-good profile and stable runtime validation used to validate the VWF patcher.

- `STATUS.json` records the claim boundary for the bucketed profile and the stable Deploy A/B runtime validation.
- `patcher_summary.json` records the EBOOT patch profile and changed ranges from the known-good run.
- `lt_summary.json` records the matching LT profile, output size, sector count, and preserved footer.

The binary artifacts that originally produced these summaries are not shipped in the source package. Recreate them from stock inputs with `README.md` commands when producing a deploy bundle.

Stable note: Deploy A confirmed the mixed custom+`ラム` cursor fix; Deploy B confirmed the same fix under continuation/wrap/fold-back conditions.
