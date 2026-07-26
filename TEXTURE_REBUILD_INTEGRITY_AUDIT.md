# Texture rebuild integrity audit — bk.cpk entry 325

## Scope

Inputs used directly:

- `bk.cpk.org` — 326-entry stock BK archive;
- uploaded `rz-tool` executable;
- `eboot.bin.elf`;
- rz-tool 1.0.1 source state containing the prior SC integrity fix.

The reproduced edit changes pixel `(512, 272)` in `00325-000.png` from
`RGBA(255,255,255,255)` to `RGBA(0,255,255,255)`. This is a diagnostic edit,
not a claim about the user's exact replacement artwork.

## Reproduction

Entry 325 has a fixed executable allocation of 546 sectors:

```text
546 * 0x800 = 0x111000 bytes
```

Its package model is:

```text
runtime surface        1024 x 544 RGBA8
post-GZIP bytes         0x220000
chunk stride            0x080000
chunk count             5
chunk table delta       0x1400
entry allocation        0x111000
```

A no-edit rebuild reproduces entry 325 byte-for-byte. A one-pixel edit remains
inside the same `0x111000` allocation, reparses, decompresses, and re-extracts
to the edited image exactly. Therefore the failure is not caused by ITOC order,
entry overlap, allocation growth, PNG dimensions, chunk destination stride, or
GZIP decode failure.

The old image encoder preserved the last 16 bytes of the original allocation:

```text
c61022978231856927f537a2f1992222
```

After the one-pixel edit, `FUN_8102B4AC` requires:

```text
53185a780ff7c24255f913de1533f7ef
```

The stale old-tool archive validates 325/326 entries; only entry 325 fails. The
corrected archive validates 326/326 entries. Repairing it changes exactly the
final 16 physical bytes of the CPK, at `0x11d877f0..0x11d877ff`.

## Engine routine chain

### BK loader

`FUN_81053CDA` obtains the executable-resident sector count, then calls the
shared reader with archive index 4 (`bk.cpk`):

```text
81053da0  adds  r1, r5, #0
81053da2  bl    0x81021034
81053da6  adds  r2, r0, #0
81053da8  adds  r1, r6, #0
81053dac  movs  r0, #4
81053dae  bl    0x81053b7a
81053db2  cmp   r0, #0
81053db4  beq   load failure
```

### Common fixed-sector reader

On completed I/O, `FUN_81053B7A` invokes the callback at
`0x8110C2F8 + 0x30 = 0x8110C328` with `(buffer, sector_count << 11)`:

```text
81053c7c  ldr   r2, [r0, #0x30]
81053c7e  lsls  r1, r6, #0xb
81053c80  adds  r0, r7, #0
81053c82  blx   r2
81053c84  cmp   r0, #1
```

The ELF value at `0x8110C328` is `0x8102B4AD`, the Thumb entry for
`FUN_8102B4AC`.

### Integrity algorithm

`FUN_8102B4AC` loads the final 16 bytes, excludes them from the iteration, and
sums the preceding allocation as two independent little-endian `u64` lanes.
Both lanes start at `0x1111111111111111`; arithmetic wraps modulo `2^64`.

```text
lane0 = 0x1111111111111111
lane1 = 0x1111111111111111
for each 16-byte block before the footer:
    lane0 += le_u64(block[0:8])
    lane1 += le_u64(block[8:16])
footer = le_u64(lane0) || le_u64(lane1)
```

The callback returns zero on mismatch. BK loading then stops before
`FUN_81053694` processes the image package, which presents as a hang or corrupt
asset even though the package itself can be parsed offline.

## Source correction

The image-package encoder now:

1. assembles/recompresses the package;
2. rejects output exceeding the executable allocation;
3. pads to the exact fixed allocation;
4. recalculates the common 16-byte footer over all preceding bytes;
5. writes the footer at allocation end;
6. verifies the rebuilt allocation before returning it.

Extraction also verifies the footer before parsing the image package. This is
fail-closed: a stale edited entry is no longer silently treated as an opaque
asset.

The correction applies to all editable fixed-sector image profiles using the
same reader: `addpt.cpk`, `bk.cpk`, `bsf.cpk`, and `pt.cpk`. Version remains
`1.0.1`.

`tools/fix_cpk_integrity.py` repairs CPKs produced by older builds without
repacking or changing ITOC metadata.

## Validation results

```text
stock bk.cpk footer validation             326/326
no-edit rebuilt bk.cpk footer validation   326/326
old one-pixel rebuild                      325/326 (entry 325 stale)
corrected one-pixel rebuild                326/326
entry 325 no-edit byte equality            PASS
corrected archive re-extraction            PASS
re-extracted edited pixel                  RGBA(0,255,255,255)
all other entry footers after repair       unchanged
source static validator                    PASS
Python fixer syntax/check mode              PASS
```

The environment has no `cargo`, `rustc`, or `rustfmt`; the Rust source could not
be compiled here. The patch was checked through source-contract validation,
Python compilation, clean patch application, binary corpus verification, and
archive re-extraction.
