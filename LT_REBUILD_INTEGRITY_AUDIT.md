# LT rebuild integrity audit — rz-tool 1.0.1

## Result

The previous conclusion that `lt.bin` bytes `0xFD440..0xFD7FF` were entirely
renderer-ignored tail data was incomplete. The glyph renderer does stop at
`0xFD43F`, but the standalone resource loader validates the complete
`0xFD800`-byte allocation before the font is accepted.

The final 16 bytes are the same two-lane integrity footer used by fixed-sector
CPK entries. Preserving that footer after changing `lt-atlas.png` makes the
startup loader reject `lt.bin`, which appears as a hang.

## Executable evidence

The standalone table record at `0x81129E58` is:

```text
resource_id = 7
byte_size   = 0x000FD800
sectors     = 0x000001FB
runtime_dst = assigned from DAT_811B98E4
```

The startup state machine begins at `0x8101A9DE`. It installs the allocated LT
buffer into the record, starts the asynchronous read, and then invokes the
common verifier after I/O completion:

```text
8101aa70  movw  r0, #0x98e4
8101aa74  movt  r0, #0x811b
8101aa78  ldr   r0, [r0]
8101aa7a  movw  r1, #0x9e58
8101aa82  movt  r1, #0x8112
8101aa86  str   r0, [r1, #0xc]      ; record.destination = DAT_811B98E4

8101abdc  movw  r1, #0x9e58
8101abe0  movt  r1, #0x8112
8101abe6  ldr   r2, [r0, r1]        ; resource ID
8101abea  ldr   r3, [r0, #8]        ; sector count
8101abf0  ldr   r4, [r0, #0xc]      ; destination
8101abf6  ldr   r5, [r1, #0x20]     ; async read entry point
8101ac00  blx   r5

8101ac54  movw  r1, #0x9e58
8101ac58  movt  r1, #0x8112
8101ac60  ldr   r3, [r0, #4]        ; exact byte size
8101ac66  ldr   r0, [r0, #0xc]      ; destination
8101ac6c  ldr   r2, [r2, #0x30]     ; pointer at 0x8110C328
8101ac70  blx   r2
```

The executable value at `0x8110C328` is `0x8102B4AD`, selecting Thumb routine
`FUN_8102B4AC`.

## Integrity algorithm

`FUN_8102B4AC` treats the final 16 bytes as two stored little-endian `u64`
values. It computes two wrapping sums over every preceding 16-byte block:

```text
lane0 = 0x1111111111111111
lane1 = 0x1111111111111111

for each 16-byte block:
    lane0 += le_u64(block[0:8])
    lane1 += le_u64(block[8:16])

footer = le_u64(lane0) || le_u64(lane1)
```

## `lt.bin.org` layout

```text
0x000000..0x0FD43F  3,602 glyphs × 0x120 bytes
0x0FD440..0x0FD7EF  0x3B0 zero/preserved tail bytes
0x0FD7F0..0x0FD7FF  0x10 integrity footer
```

Stored footer:

```text
71f70d807e60ce6c9129df8e1a01f723
```

The algorithm above reproduces it exactly.

## Reproduction of the old bug

The stock tool extracted the atlas and rebuilt the unedited project
byte-for-byte. A test then changed atlas pixel `(0,0)` from alpha `0` to `255`.
The encoded glyph bank changed only byte `0x000000` from `00` to `0F`.

Old builder result:

```text
changed glyph byte:  0x000000: 00 -> 0F
stored footer:       71f70d807e60ce6c9129df8e1a01f723
required footer:     80f70d807e60ce6c9129df8e1a01f723
integrity result:    FAIL
```

After footer regeneration, the fixed file differs from stock at only two byte
positions: the edited glyph byte and footer byte `0x0FD7F0`.

## Source correction

`codec/lt_font.rs` now:

1. verifies the source footer before extraction;
2. retains project-schema compatibility with the existing `tail_hex` field;
3. uses only the first `0x3B0` tail bytes from that field;
4. never copies the extracted final 16 bytes;
5. encodes all glyph data;
6. pads to exactly `0xFD800` bytes;
7. regenerates the common integrity footer;
8. verifies the rebuilt footer before returning the output.

The workspace and project versions remain `1.0.1` and schema `1`.

## Repairing files built by the old encoder

```bash
python tools/fix_lt_integrity.py broken-lt.bin fixed-lt.bin
python tools/fix_lt_integrity.py fixed-lt.bin --check
```

The repair changes only the final 16-byte footer and does not re-encode glyphs.

## Validation

```text
stock size                                    0xFD800
stock footer verification                    PASS
no-edit old-tool rebuild byte-exact           PASS
one-pixel old-tool rebuild footer             FAIL
one-pixel footer-regenerated rebuild          PASS
re-extracted edited pixel                     RGBA(255,255,255,255)
static source validator                       PASS
Capstone evidence regeneration                PASS
workspace version                             1.0.1
```

A Rust toolchain was not present in the analysis environment, so `cargo check`,
`cargo test`, and a new executable build remain release gates.
