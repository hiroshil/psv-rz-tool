use std::fs;
use std::path::Path;

use image::{Rgba, RgbaImage};
use serde::{Deserialize, Serialize};

use crate::codec::engine_package::{
    regenerate_engine_integrity_footer, verify_engine_integrity_footer,
    ENGINE_INTEGRITY_FOOTER_SIZE,
};
use crate::error::AssetError;

const DOCUMENT_VERSION: u32 = 1;
const STOCK_GLYPH_COUNT: usize = 0x0e12;
const GLYPH_WIDTH: usize = 24;
const GLYPH_HEIGHT: usize = 24;
const GLYPH_BYTES: usize = GLYPH_WIDTH * GLYPH_HEIGHT / 2;
const STOCK_TAIL_SIZE: usize = 0x3c0;
const LT_FILE_ALIGNMENT: usize = 0x800;
const DEFAULT_COLUMNS: usize = 64;
const VWF_GLYPH_BUCKET_SIZE: usize = 0x40;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LtTailPolicy {
    /// Preserve the extracted non-glyph tail bytes when possible and regenerate
    /// the integrity footer. Expanded VWF output may require more aligned tail
    /// space than the source project contains; the additional bytes are zero.
    PreserveInline,
    /// Recreate the non-glyph tail as zero and regenerate the footer.
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
    /// Number of tail bytes extracted into `tail_hex`. This may be smaller than
    /// the final aligned tail when `glyph_count` is expanded by the VWF profile.
    pub tail_bytes: u32,
    /// Hex-encoded non-glyph tail bytes, including the original 16-byte footer.
    /// The footer is never copied to rebuilt output; it is regenerated after
    /// atlas/tail bytes have been materialized.
    pub tail_hex: Option<String>,
}

pub fn decode(input: &[u8], output_directory: &Path) -> Result<String, AssetError> {
    let glyph_count = infer_glyph_count(input.len())?;
    let glyph_data_size = glyph_count.checked_mul(GLYPH_BYTES).ok_or_else(|| {
        AssetError::InvalidFormat("lt.bin glyph data size overflows usize".to_owned())
    })?;
    let tail_size = input.len().checked_sub(glyph_data_size).ok_or_else(|| {
        AssetError::InvalidFormat("lt.bin is shorter than its inferred glyph data".to_owned())
    })?;
    if tail_size < ENGINE_INTEGRITY_FOOTER_SIZE {
        return Err(AssetError::InvalidFormat(format!(
            "lt.bin tail is {tail_size:#x} bytes; expected at least the 16-byte integrity footer"
        )));
    }
    verify_engine_integrity_footer(input).map_err(|error| {
        AssetError::InvalidFormat(format!("lt.bin integrity footer mismatch: {error}"))
    })?;

    let rows = glyph_count.div_ceil(DEFAULT_COLUMNS);
    let mut atlas = RgbaImage::new(
        u32::try_from(DEFAULT_COLUMNS * GLYPH_WIDTH).unwrap(),
        u32::try_from(rows * GLYPH_HEIGHT).unwrap(),
    );
    for glyph_index in 0..glyph_count {
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

    // FUN_8101b504 allocates the byte count stored in the executable LT table.
    // Stock is 0x0e12 glyphs and 0xfd800 bytes. The VWF profile expands the
    // runtime glyph limit in 0x40 buckets and sector-aligns the final lt.bin
    // allocation; the renderer still addresses glyph_id * 0x120 through +0x11f.
    // The final 0x10 bytes are the shared engine integrity footer and must be
    // regenerated on build.
    let document = LtFontDocument {
        document_version: DOCUMENT_VERSION,
        glyph_count: u32::try_from(glyph_count).unwrap(),
        glyph_width: u32::try_from(GLYPH_WIDTH).unwrap(),
        glyph_height: u32::try_from(GLYPH_HEIGHT).unwrap(),
        atlas_columns: u32::try_from(DEFAULT_COLUMNS).unwrap(),
        atlas: atlas_name.to_owned(),
        tail_policy: LtTailPolicy::PreserveInline,
        tail_bytes: u32::try_from(tail_size).unwrap(),
        tail_hex: Some(encode_hex(&input[glyph_data_size..])),
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
        || document.glyph_width != u32::try_from(GLYPH_WIDTH).unwrap()
        || document.glyph_height != u32::try_from(GLYPH_HEIGHT).unwrap()
    {
        return Err(AssetError::InvalidProject(
            "lt.bin glyph geometry must be document_version=1 and 24x24 4bpp".to_owned(),
        ));
    }
    let glyph_count = usize::try_from(document.glyph_count).map_err(|_| {
        AssetError::InvalidProject("lt.bin glyph_count overflows usize".to_owned())
    })?;
    if glyph_count < STOCK_GLYPH_COUNT {
        return Err(AssetError::InvalidProject(format!(
            "lt.bin glyph_count {glyph_count:#x} is smaller than stock {STOCK_GLYPH_COUNT:#x}"
        )));
    }
    let columns = usize::try_from(document.atlas_columns).map_err(|_| {
        AssetError::InvalidProject("font atlas column count overflows usize".to_owned())
    })?;
    if columns == 0 {
        return Err(AssetError::InvalidProject(
            "font atlas column count is zero".to_owned(),
        ));
    }
    let glyph_data_size = glyph_count.checked_mul(GLYPH_BYTES).ok_or_else(|| {
        AssetError::InvalidProject("lt.bin glyph data size overflows usize".to_owned())
    })?;
    let tail_bytes = usize::try_from(document.tail_bytes).map_err(|_| {
        AssetError::InvalidProject("lt.bin tail_bytes overflows usize".to_owned())
    })?;
    let output_size = aligned_lt_file_size(glyph_count, tail_bytes).map_err(|error| {
        AssetError::InvalidProject(format!("failed to derive lt.bin file size: {error}"))
    })?;
    let final_tail_size = output_size.checked_sub(glyph_data_size).ok_or_else(|| {
        AssetError::InvalidProject("lt.bin aligned file size is smaller than glyph data".to_owned())
    })?;
    if final_tail_size < ENGINE_INTEGRITY_FOOTER_SIZE {
        return Err(AssetError::InvalidProject(format!(
            "lt.bin final tail is {final_tail_size:#x} bytes; expected at least the 16-byte integrity footer"
        )));
    }

    let rows = glyph_count.div_ceil(columns);
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

    let tail_payload_len = final_tail_size - ENGINE_INTEGRITY_FOOTER_SIZE;
    let tail_payload = match document.tail_policy {
        LtTailPolicy::PreserveInline => {
            let encoded = document.tail_hex.as_deref().ok_or_else(|| {
                AssetError::InvalidProject(
                    "lt.bin preserve-inline policy requires tail_hex".to_owned(),
                )
            })?;
            let decoded = decode_hex(encoded)?;
            let declared_tail = usize::try_from(document.tail_bytes).map_err(|_| {
                AssetError::InvalidProject("lt.bin tail_bytes overflows usize".to_owned())
            })?;
            if decoded.len() != declared_tail {
                return Err(AssetError::InvalidProject(format!(
                    "lt.bin inline tail is {} bytes, expected tail_bytes={declared_tail:#x}",
                    decoded.len()
                )));
            }
            let non_footer_tail = decoded.len().saturating_sub(ENGINE_INTEGRITY_FOOTER_SIZE);
            let preserve_len = non_footer_tail.min(tail_payload_len);
            let mut payload = vec![0u8; tail_payload_len];
            payload[..preserve_len].copy_from_slice(&decoded[..preserve_len]);
            payload
        }
        LtTailPolicy::ZeroFill => vec![0u8; tail_payload_len],
    };

    let mut output = vec![0u8; glyph_data_size];
    for glyph_index in 0..glyph_count {
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
    output.extend_from_slice(&tail_payload);
    output.resize(output_size, 0);
    regenerate_engine_integrity_footer(&mut output).map_err(|error| {
        AssetError::InvalidProject(format!(
            "failed to generate lt.bin integrity footer: {error}"
        ))
    })?;
    verify_engine_integrity_footer(&output).map_err(|error| {
        AssetError::InvalidProject(format!(
            "rebuilt lt.bin failed integrity verification: {error}"
        ))
    })?;
    debug_assert_eq!(output.len(), output_size);
    Ok(output)
}

fn infer_glyph_count(file_size: usize) -> Result<usize, AssetError> {
    if file_size == aligned_lt_file_size(STOCK_GLYPH_COUNT, STOCK_TAIL_SIZE).map_err(|error| {
        AssetError::InvalidFormat(format!("failed to derive stock lt.bin size: {error}"))
    })? {
        return Ok(STOCK_GLYPH_COUNT);
    }
    if file_size % LT_FILE_ALIGNMENT != 0 {
        return Err(AssetError::InvalidFormat(format!(
            "lt.bin is {file_size:#x} bytes; expected stock size or an expanded sector-aligned VWF size"
        )));
    }
    let mut candidate = align_up(STOCK_GLYPH_COUNT, VWF_GLYPH_BUCKET_SIZE).map_err(|error| {
        AssetError::InvalidFormat(format!("failed to derive first VWF lt.bin bucket: {error}"))
    })?;
    while candidate <= 0x10000 {
        if aligned_lt_file_size(candidate, STOCK_TAIL_SIZE).map_err(|error| {
            AssetError::InvalidFormat(format!("failed to derive expanded lt.bin size: {error}"))
        })? == file_size
        {
            return Ok(candidate);
        }
        candidate = candidate.saturating_add(VWF_GLYPH_BUCKET_SIZE);
    }
    Err(AssetError::InvalidFormat(format!(
        "lt.bin size {file_size:#x} does not match a supported stock or bucketed VWF glyph profile"
    )))
}

fn aligned_lt_file_size(glyph_count: usize, tail_bytes: usize) -> Result<usize, String> {
    let glyph_bytes = glyph_count
        .checked_mul(GLYPH_BYTES)
        .ok_or_else(|| "lt.bin glyph byte count overflows usize".to_owned())?;
    let minimum_tail = tail_bytes.max(STOCK_TAIL_SIZE);
    let minimum = glyph_bytes
        .checked_add(minimum_tail)
        .ok_or_else(|| "lt.bin file size overflows usize".to_owned())?;
    align_up(minimum, LT_FILE_ALIGNMENT)
}

fn align_up(value: usize, alignment: usize) -> Result<usize, String> {
    if alignment == 0 || !alignment.is_power_of_two() {
        return Err("alignment must be a non-zero power of two".to_owned());
    }
    value
        .checked_add(alignment - 1)
        .map(|sum| sum & !(alignment - 1))
        .ok_or_else(|| "alignment overflow".to_owned())
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
    fn stock_font_geometry_matches_engine_file_size() {
        assert_eq!(
            aligned_lt_file_size(STOCK_GLYPH_COUNT, STOCK_TAIL_SIZE).unwrap(),
            0x0fd800
        );
    }

    #[test]
    fn bucketed_vwf_font_geometry_matches_profile_size() {
        assert_eq!(aligned_lt_file_size(0x0f40, STOCK_TAIL_SIZE).unwrap(), 0x113000);
        assert_eq!(infer_glyph_count(0x113000).unwrap(), 0x0f40);
    }

    #[test]
    fn expanded_tail_zero_fill_size_matches_aligned_profile() {
        assert_eq!(
            aligned_lt_file_size(0x0f40, STOCK_TAIL_SIZE).unwrap() - 0x0f40 * GLYPH_BYTES,
            0x800
        );
    }
}
