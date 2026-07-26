use std::fs;
use std::path::Path;

use image::{Rgba, RgbaImage};
use serde::{Deserialize, Serialize};

use crate::error::AssetError;

const DOCUMENT_VERSION: u32 = 1;
const GLYPH_COUNT: usize = 0x0e12;
const GLYPH_WIDTH: usize = 24;
const GLYPH_HEIGHT: usize = 24;
const GLYPH_BYTES: usize = GLYPH_WIDTH * GLYPH_HEIGHT / 2;
const GLYPH_DATA_SIZE: usize = GLYPH_COUNT * GLYPH_BYTES;
const FILE_SIZE: usize = 0x0fd800;
const TAIL_SIZE: usize = FILE_SIZE - GLYPH_DATA_SIZE;
const DEFAULT_COLUMNS: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LtTailPolicy {
    /// Preserve the original non-glyph bytes inline for byte-identical rebuild.
    PreserveInline,
    /// Recreate the engine-ignored sector padding as zero bytes.
    ZeroFill,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LtFontDocument {
    pub document_version: u32,
    pub glyph_count: u32,
    pub glyph_width: u32,
    pub glyph_height: u32,
    pub atlas_columns: u32,
    pub atlas: String,
    pub tail_policy: LtTailPolicy,
    pub tail_bytes: u32,
    /// Hex-encoded bytes 0xfd440..0xfd7ff. The field is inline so extraction
    /// does not create a separate lt-tail.bin. It is required only for exact
    /// binary round-trip; the analyzed renderer never addresses this range.
    pub tail_hex: Option<String>,
}

pub fn decode(input: &[u8], output_directory: &Path) -> Result<String, AssetError> {
    if input.len() != FILE_SIZE {
        return Err(AssetError::InvalidFormat(format!(
            "lt.bin is {} bytes; engine requires exactly {FILE_SIZE:#x}",
            input.len()
        )));
    }
    let rows = GLYPH_COUNT.div_ceil(DEFAULT_COLUMNS);
    let mut atlas = RgbaImage::new(
        u32::try_from(DEFAULT_COLUMNS * GLYPH_WIDTH).unwrap(),
        u32::try_from(rows * GLYPH_HEIGHT).unwrap(),
    );
    for glyph_index in 0..GLYPH_COUNT {
        let source = glyph_index * GLYPH_BYTES;
        let tile_x = (glyph_index % DEFAULT_COLUMNS) * GLYPH_WIDTH;
        let tile_y = (glyph_index / DEFAULT_COLUMNS) * GLYPH_HEIGHT;
        for pixel_index in 0..(GLYPH_WIDTH * GLYPH_HEIGHT) {
            let packed = input[source + pixel_index / 2];
            let nibble = if pixel_index & 1 == 0 {
                packed & 0x0f
            } else {
                packed >> 4
            };
            let x = tile_x + pixel_index % GLYPH_WIDTH;
            let y = tile_y + pixel_index / GLYPH_WIDTH;
            atlas.put_pixel(
                u32::try_from(x).unwrap(),
                u32::try_from(y).unwrap(),
                Rgba([255, 255, 255, nibble.saturating_mul(17)]),
            );
        }
    }
    let atlas_name = "lt-atlas.png";
    atlas.save(output_directory.join(atlas_name))?;

    // FUN_8101b504 allocates exactly 0xfd800 bytes and FUN_8101a9c0 loads
    // exactly 0x1fb sectors. The function-pointer table at 0x8110c470 points
    // directly to FUN_8102d78c/FUN_8102d194. A whole-ELF immediate-reference
    // census found DAT_811b98e4 only in allocation/loading and renderer-family
    // call paths; the string rasterizer FUN_8104db86 dispatches through the
    // same table entry. The 24px renderer rejects glyph_id >= 0x0e12 and reads
    // glyph_id * 0x120 through +0x11f, so its maximum read ends at 0xfd43f.
    // No located consumer, producer, or checksum routine addresses
    // 0xfd440..0xfd7ff. Zero-fill is therefore the strongest static functional
    // reconstruction; preserving the bytes remains necessary for exact binary
    // identity because their original values cannot be derived.
    let document = LtFontDocument {
        document_version: DOCUMENT_VERSION,
        glyph_count: u32::try_from(GLYPH_COUNT).unwrap(),
        glyph_width: u32::try_from(GLYPH_WIDTH).unwrap(),
        glyph_height: u32::try_from(GLYPH_HEIGHT).unwrap(),
        atlas_columns: u32::try_from(DEFAULT_COLUMNS).unwrap(),
        atlas: atlas_name.to_owned(),
        tail_policy: LtTailPolicy::PreserveInline,
        tail_bytes: u32::try_from(TAIL_SIZE).unwrap(),
        tail_hex: Some(encode_hex(&input[GLYPH_DATA_SIZE..])),
    };
    let document_name = "lt-font.json";
    fs::write(
        output_directory.join(document_name),
        serde_json::to_vec_pretty(&document)?,
    )?;
    Ok(document_name.to_owned())
}

pub fn encode(document_path: &Path) -> Result<Vec<u8>, AssetError> {
    let document: LtFontDocument = serde_json::from_slice(&fs::read(document_path)?)?;
    if document.document_version != DOCUMENT_VERSION
        || document.glyph_count != u32::try_from(GLYPH_COUNT).unwrap()
        || document.glyph_width != u32::try_from(GLYPH_WIDTH).unwrap()
        || document.glyph_height != u32::try_from(GLYPH_HEIGHT).unwrap()
        || document.tail_bytes != u32::try_from(TAIL_SIZE).unwrap()
    {
        return Err(AssetError::InvalidProject(
            "lt.bin geometry differs from the fixed layout used by the engine".to_owned(),
        ));
    }
    let columns = usize::try_from(document.atlas_columns).map_err(|_| {
        AssetError::InvalidProject("font atlas column count overflows usize".to_owned())
    })?;
    if columns == 0 {
        return Err(AssetError::InvalidProject(
            "font atlas column count is zero".to_owned(),
        ));
    }
    let rows = GLYPH_COUNT.div_ceil(columns);
    let root = document_path.parent().unwrap_or_else(|| Path::new("."));
    let atlas = image::open(root.join(&document.atlas))?.to_rgba8();
    let expected_width = u32::try_from(columns * GLYPH_WIDTH).unwrap();
    let expected_height = u32::try_from(rows * GLYPH_HEIGHT).unwrap();
    if atlas.width() != expected_width || atlas.height() != expected_height {
        return Err(AssetError::InvalidProject(format!(
            "font atlas is {}x{}, expected {expected_width}x{expected_height}",
            atlas.width(),
            atlas.height()
        )));
    }

    let tail = match document.tail_policy {
        LtTailPolicy::PreserveInline => {
            let encoded = document.tail_hex.as_deref().ok_or_else(|| {
                AssetError::InvalidProject(
                    "lt.bin preserve-inline policy requires tail_hex".to_owned(),
                )
            })?;
            let decoded = decode_hex(encoded)?;
            if decoded.len() != TAIL_SIZE {
                return Err(AssetError::InvalidProject(format!(
                    "lt.bin inline tail is {} bytes, expected {TAIL_SIZE:#x}",
                    decoded.len()
                )));
            }
            decoded
        }
        LtTailPolicy::ZeroFill => vec![0u8; TAIL_SIZE],
    };

    let mut output = vec![0u8; GLYPH_DATA_SIZE];
    for glyph_index in 0..GLYPH_COUNT {
        let destination = glyph_index * GLYPH_BYTES;
        let tile_x = (glyph_index % columns) * GLYPH_WIDTH;
        let tile_y = (glyph_index / columns) * GLYPH_HEIGHT;
        for pair in 0..GLYPH_BYTES {
            let pixel0 = pair * 2;
            let x0 = tile_x + pixel0 % GLYPH_WIDTH;
            let y0 = tile_y + pixel0 / GLYPH_WIDTH;
            let pixel1 = pixel0 + 1;
            let x1 = tile_x + pixel1 % GLYPH_WIDTH;
            let y1 = tile_y + pixel1 / GLYPH_WIDTH;
            let low = quantize_alpha(
                atlas.get_pixel(u32::try_from(x0).unwrap(), u32::try_from(y0).unwrap()).0[3],
            );
            let high = quantize_alpha(
                atlas.get_pixel(u32::try_from(x1).unwrap(), u32::try_from(y1).unwrap()).0[3],
            );
            output[destination + pair] = low | (high << 4);
        }
    }
    output.extend_from_slice(&tail);
    debug_assert_eq!(output.len(), FILE_SIZE);
    Ok(output)
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn decode_hex(value: &str) -> Result<Vec<u8>, AssetError> {
    if value.len() % 2 != 0 {
        return Err(AssetError::InvalidProject(
            "lt.bin tail_hex has odd length".to_owned(),
        ));
    }
    let mut output = Vec::with_capacity(value.len() / 2);
    let bytes = value.as_bytes();
    for index in (0..bytes.len()).step_by(2) {
        let high = hex_nibble(bytes[index]).ok_or_else(|| {
            AssetError::InvalidProject(format!(
                "lt.bin tail_hex contains invalid byte at character {index}"
            ))
        })?;
        let low = hex_nibble(bytes[index + 1]).ok_or_else(|| {
            AssetError::InvalidProject(format!(
                "lt.bin tail_hex contains invalid byte at character {}",
                index + 1
            ))
        })?;
        output.push((high << 4) | low);
    }
    Ok(output)
}

fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn quantize_alpha(alpha: u8) -> u8 {
    ((u16::from(alpha) + 8) / 17).min(15) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_font_geometry_matches_engine_file_size() {
        assert_eq!(GLYPH_DATA_SIZE, 0x0fd440);
        assert_eq!(TAIL_SIZE, 0x3c0);
        assert_eq!(0x1fb * 0x800, FILE_SIZE);
    }

    #[test]
    fn inline_tail_hex_round_trips() {
        let bytes = (0u8..=255).collect::<Vec<_>>();
        assert_eq!(decode_hex(&encode_hex(&bytes)).unwrap(), bytes);
    }
}
