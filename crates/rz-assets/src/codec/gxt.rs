use std::path::Path;

use image::{Rgba, RgbaImage};

use crate::error::AssetError;
use crate::manifest::{AssetKind, GxtMetadata, GxtTextureMetadata};

const GXT_HEADER_SIZE: usize = 0x20;
const GXT_TEXTURE_INFO_SIZE: usize = 0x20;
const GXT_TAG: u32 = 0x0054_5847;
const GXT_VERSION_3: u32 = 0x1000_0003;

const TEXTURE_TYPE_SWIZZLED: u32 = 0x0000_0000;
const TEXTURE_TYPE_CUBE: u32 = 0x4000_0000;
const TEXTURE_TYPE_LINEAR: u32 = 0x6000_0000;
const TEXTURE_TYPE_TILED: u32 = 0x8000_0000;
const TEXTURE_TYPE_SWIZZLED_ARBITRARY: u32 = 0xa000_0000;
const TEXTURE_TYPE_LINEAR_STRIDED: u32 = 0xc000_0000;
const TEXTURE_TYPE_CUBE_ARBITRARY: u32 = 0xe000_0000;

const FORMAT_BASE_MASK: u32 = 0xff00_0000;
const FORMAT_SWIZZLE_MASK: u32 = 0x0000_f000;
const FORMAT_U8U8U8U8: u32 = 0x0c00_0000;
const FORMAT_BC1: u32 = 0x8500_0000;
const FORMAT_BC2: u32 = 0x8600_0000;
const FORMAT_BC3: u32 = 0x8700_0000;

#[derive(Debug, Clone, Copy)]
struct Header {
    version: u32,
    texture_count: u32,
    data_offset: u32,
    data_size: u32,
    p4_palettes: u32,
    p8_palettes: u32,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct EngineTextureLayout {
    pub palette_index: u32,
    pub flags: u32,
    pub texture_type: u32,
    pub format: u32,
    pub width: u16,
    pub height: u16,
    pub mip_count: u8,
}

#[derive(Debug, Clone, Copy)]
struct TextureInfo {
    data_offset: u32,
    data_size: u32,
    palette_index: u32,
    flags: u32,
    texture_type: u32,
    format: u32,
    width: u16,
    height: u16,
    mip_count: u8,
}

pub fn decode_engine_texture(
    input: &[u8],
    layout: EngineTextureLayout,
) -> Result<RgbaImage, AssetError> {
    let info = TextureInfo {
        data_offset: 0,
        data_size: u32::try_from(input.len()).map_err(|_| {
            AssetError::InvalidFormat("engine texture exceeds u32".to_owned())
        })?,
        palette_index: layout.palette_index,
        flags: layout.flags,
        texture_type: layout.texture_type,
        format: layout.format,
        width: layout.width,
        height: layout.height,
        mip_count: layout.mip_count,
    };
    validate_texture_info(info)?;
    let (storage_width, storage_height) = infer_storage_extent(info)?;
    decode_texture(input, info, storage_width, storage_height)
}

pub fn encode_engine_texture(
    image: &RgbaImage,
    layout: EngineTextureLayout,
    expected_size: usize,
) -> Result<Vec<u8>, AssetError> {
    if image.width() != u32::from(layout.width) || image.height() != u32::from(layout.height) {
        return Err(AssetError::InvalidProject(format!(
            "engine texture is {}x{}, expected {}x{}",
            image.width(), image.height(), layout.width, layout.height
        )));
    }
    let info = TextureInfo {
        data_offset: 0,
        data_size: u32::try_from(expected_size).map_err(|_| {
            AssetError::InvalidProject("engine texture exceeds u32".to_owned())
        })?,
        palette_index: layout.palette_index,
        flags: layout.flags,
        texture_type: layout.texture_type,
        format: layout.format,
        width: layout.width,
        height: layout.height,
        mip_count: layout.mip_count,
    };
    validate_texture_info(info)?;
    let (storage_width, storage_height) = infer_storage_extent(info)
        .map_err(|error| AssetError::InvalidProject(error.to_string()))?;
    let encoded = encode_texture(image, info, storage_width, storage_height)?;
    if encoded.len() != expected_size {
        return Err(AssetError::InvalidProject(format!(
            "encoded engine texture has {} bytes, expected {expected_size}",
            encoded.len()
        )));
    }
    Ok(encoded)
}

/// Encode an engine texture while preserving exact source storage units that
/// still decode to the edited image. This is the safe editable path for engine
/// packages: unchanged BC blocks and padding texels remain byte-identical, while
/// changed blocks are encoded in the original physical layout.
pub fn encode_engine_texture_preserving_source(
    image: &RgbaImage,
    layout: EngineTextureLayout,
    expected_size: usize,
    source: &[u8],
) -> Result<Vec<u8>, AssetError> {
    if image.width() != u32::from(layout.width) || image.height() != u32::from(layout.height) {
        return Err(AssetError::InvalidProject(format!(
            "engine texture is {}x{}, expected {}x{}",
            image.width(), image.height(), layout.width, layout.height
        )));
    }
    if source.len() != expected_size {
        return Err(AssetError::InvalidProject(format!(
            "source engine texture has {} bytes, expected {expected_size}",
            source.len()
        )));
    }
    let info = TextureInfo {
        data_offset: 0,
        data_size: u32::try_from(expected_size).map_err(|_| {
            AssetError::InvalidProject("engine texture exceeds u32".to_owned())
        })?,
        palette_index: layout.palette_index,
        flags: layout.flags,
        texture_type: layout.texture_type,
        format: layout.format,
        width: layout.width,
        height: layout.height,
        mip_count: layout.mip_count,
    };
    validate_texture_info(info)?;
    let (storage_width, storage_height) = infer_storage_extent(info)
        .map_err(|error| AssetError::InvalidProject(error.to_string()))?;
    encode_texture_preserving_source(image, info, storage_width, storage_height, source)
}


pub fn decode(
    input: &[u8],
    asset_directory: &Path,
    output_stem: &str,
) -> Result<AssetKind, AssetError> {
    let header = parse_header(input)?;
    if header.version != GXT_VERSION_3 {
        return Err(AssetError::InvalidFormat(format!(
            "GXT version {:#010x} uses a different texture descriptor layout; editable mode currently supports only {GXT_VERSION_3:#010x}; use --raw-only",
            header.version
        )));
    }
    if header.p4_palettes != 0 || header.p8_palettes != 0 {
        return Err(AssetError::InvalidFormat(
            "paletted GXT is not supported in editable mode; use --raw-only".to_owned(),
        ));
    }
    let count = usize::try_from(header.texture_count)
        .map_err(|_| AssetError::InvalidFormat("GXT texture count overflows usize".to_owned()))?;
    let descriptors_end = GXT_HEADER_SIZE
        .checked_add(count.checked_mul(GXT_TEXTURE_INFO_SIZE).ok_or_else(|| {
            AssetError::InvalidFormat("GXT descriptor size overflow".to_owned())
        })?)
        .ok_or_else(|| AssetError::InvalidFormat("GXT descriptor offset overflow".to_owned()))?;
    if descriptors_end > input.len() {
        return Err(AssetError::InvalidFormat("truncated GXT texture table".to_owned()));
    }
    let data_start = usize::try_from(header.data_offset)
        .map_err(|_| AssetError::InvalidFormat("GXT data offset overflows usize".to_owned()))?;
    let data_size = usize::try_from(header.data_size)
        .map_err(|_| AssetError::InvalidFormat("GXT data size overflows usize".to_owned()))?;
    let data_end = data_start
        .checked_add(data_size)
        .ok_or_else(|| AssetError::InvalidFormat("GXT data range overflow".to_owned()))?;
    if data_start < descriptors_end || data_end > input.len() {
        return Err(AssetError::InvalidFormat("invalid GXT data section".to_owned()));
    }

    let mut images = Vec::with_capacity(count);
    let mut metadata = Vec::with_capacity(count);
    for index in 0..count {
        let offset = GXT_HEADER_SIZE + index * GXT_TEXTURE_INFO_SIZE;
        let info = parse_texture_info(&input[offset..offset + GXT_TEXTURE_INFO_SIZE])?;
        validate_texture_info(info)?;
        let start = usize::try_from(info.data_offset)
            .map_err(|_| AssetError::InvalidFormat("GXT texture offset overflows usize".to_owned()))?;
        let texture_size = usize::try_from(info.data_size)
            .map_err(|_| AssetError::InvalidFormat("GXT texture size overflows usize".to_owned()))?;
        let end = start
            .checked_add(texture_size)
            .ok_or_else(|| AssetError::InvalidFormat("GXT texture size overflow".to_owned()))?;
        if start < data_start || end > data_end {
            return Err(AssetError::InvalidFormat(format!(
                "GXT texture {index} exceeds the data section"
            )));
        }
        let (storage_width, storage_height) = infer_storage_extent(info)?;
        let image = decode_texture(
            &input[start..end],
            info,
            storage_width,
            storage_height,
        )?;
        let relative = format!("{output_stem}-{index:03}.png");
        image.save(asset_directory.join(&relative))?;
        images.push(relative);
        metadata.push(GxtTextureMetadata {
            palette_index: info.palette_index,
            flags: info.flags,
            texture_type: info.texture_type,
            format: info.format,
            width: info.width,
            height: info.height,
            storage_width,
            storage_height,
            mip_count: info.mip_count,
        });
    }

    Ok(AssetKind::Gxt {
        images,
        metadata: GxtMetadata {
            version: header.version,
            textures: metadata,
        },
    })
}

pub fn encode(
    project_root: &Path,
    image_paths: &[String],
    metadata: &GxtMetadata,
) -> Result<Vec<u8>, AssetError> {
    if metadata.version != GXT_VERSION_3 {
        return Err(AssetError::InvalidProject(format!(
            "GXT version {:#010x} cannot be encoded by the version-3 serializer",
            metadata.version
        )));
    }
    if image_paths.len() != metadata.textures.len() {
        return Err(AssetError::InvalidProject(
            "GXT image count differs from texture metadata".to_owned(),
        ));
    }
    if image_paths.is_empty() {
        return Err(AssetError::InvalidProject("GXT has no textures".to_owned()));
    }

    let descriptor_end = GXT_HEADER_SIZE
        .checked_add(image_paths.len().checked_mul(GXT_TEXTURE_INFO_SIZE).ok_or_else(|| {
            AssetError::InvalidProject("GXT descriptor size overflow".to_owned())
        })?)
        .ok_or_else(|| AssetError::InvalidProject("GXT descriptor offset overflow".to_owned()))?;
    let data_offset = checked_align_up(descriptor_end, 0x10)
        .ok_or_else(|| AssetError::InvalidProject("GXT data offset overflow".to_owned()))?;
    let mut texture_bytes = Vec::<Vec<u8>>::with_capacity(image_paths.len());
    for (path, texture) in image_paths.iter().zip(metadata.textures.iter()) {
        let image = image::open(project_root.join(path))?.to_rgba8();
        if image.width() != u32::from(texture.width) || image.height() != u32::from(texture.height) {
            return Err(AssetError::InvalidProject(format!(
                "{} is {}x{}, expected {}x{}",
                path,
                image.width(),
                image.height(),
                texture.width,
                texture.height
            )));
        }
        let info = TextureInfo {
            data_offset: 0,
            data_size: 0,
            palette_index: texture.palette_index,
            flags: texture.flags,
            texture_type: texture.texture_type,
            format: texture.format,
            width: texture.width,
            height: texture.height,
            mip_count: texture.mip_count,
        };
        validate_texture_info(info)?;
        if let Some(message) = storage_extent_violation(
            info,
            texture.storage_width,
            texture.storage_height,
        ) {
            return Err(AssetError::InvalidProject(message.to_owned()));
        }
        texture_bytes.push(encode_texture(
            &image,
            info,
            texture.storage_width,
            texture.storage_height,
        )?);
    }

    let mut offsets = Vec::with_capacity(texture_bytes.len());
    let mut data = Vec::new();
    for bytes in &texture_bytes {
        let aligned = checked_align_up(data.len(), 0x10)
            .ok_or_else(|| AssetError::InvalidProject("GXT texture alignment overflow".to_owned()))?;
        data.resize(aligned, 0);
        offsets.push(u32::try_from(data.len()).map_err(|_| {
            AssetError::InvalidProject("GXT texture data exceeds u32".to_owned())
        })?);
        data.extend_from_slice(bytes);
    }

    let data_offset_u32 = u32::try_from(data_offset)
        .map_err(|_| AssetError::InvalidProject("GXT header exceeds u32".to_owned()))?;
    let data_size_u32 = u32::try_from(data.len())
        .map_err(|_| AssetError::InvalidProject("GXT data exceeds u32".to_owned()))?;
    let texture_count = u32::try_from(texture_bytes.len())
        .map_err(|_| AssetError::InvalidProject("too many GXT textures".to_owned()))?;

    let output_capacity = data_offset
        .checked_add(data.len())
        .ok_or_else(|| AssetError::InvalidProject("GXT output size overflow".to_owned()))?;
    let mut output = Vec::with_capacity(output_capacity);
    push_u32_le(&mut output, GXT_TAG);
    push_u32_le(&mut output, metadata.version);
    push_u32_le(&mut output, texture_count);
    push_u32_le(&mut output, data_offset_u32);
    push_u32_le(&mut output, data_size_u32);
    push_u32_le(&mut output, 0);
    push_u32_le(&mut output, 0);
    push_u32_le(&mut output, 0);

    for ((texture, bytes), relative_offset) in metadata
        .textures
        .iter()
        .zip(texture_bytes.iter())
        .zip(offsets.iter())
    {
        let absolute_offset = data_offset_u32
            .checked_add(*relative_offset)
            .ok_or_else(|| AssetError::InvalidProject("GXT texture offset exceeds u32".to_owned()))?;
        push_u32_le(&mut output, absolute_offset);
        push_u32_le(
            &mut output,
            u32::try_from(bytes.len()).map_err(|_| {
                AssetError::InvalidProject("GXT texture exceeds u32".to_owned())
            })?,
        );
        push_u32_le(&mut output, texture.palette_index);
        push_u32_le(&mut output, texture.flags);
        push_u32_le(&mut output, texture.texture_type);
        push_u32_le(&mut output, texture.format);
        output.extend_from_slice(&texture.width.to_le_bytes());
        output.extend_from_slice(&texture.height.to_le_bytes());
        output.push(texture.mip_count);
        output.extend_from_slice(&[0, 0, 0]);
    }
    output.resize(data_offset, 0);
    output.extend_from_slice(&data);
    Ok(output)
}

fn parse_header(input: &[u8]) -> Result<Header, AssetError> {
    if input.len() < GXT_HEADER_SIZE {
        return Err(AssetError::InvalidFormat("truncated GXT header".to_owned()));
    }
    if read_u32_le(input, 0)? != GXT_TAG {
        return Err(AssetError::InvalidFormat("invalid GXT tag".to_owned()));
    }
    Ok(Header {
        version: read_u32_le(input, 4)?,
        texture_count: read_u32_le(input, 8)?,
        data_offset: read_u32_le(input, 12)?,
        data_size: read_u32_le(input, 16)?,
        p4_palettes: read_u32_le(input, 20)?,
        p8_palettes: read_u32_le(input, 24)?,
    })
}

fn parse_texture_info(input: &[u8]) -> Result<TextureInfo, AssetError> {
    if input.len() < GXT_TEXTURE_INFO_SIZE {
        return Err(AssetError::InvalidFormat("truncated GXT texture info".to_owned()));
    }
    Ok(TextureInfo {
        data_offset: read_u32_le(input, 0)?,
        data_size: read_u32_le(input, 4)?,
        palette_index: read_u32_le(input, 8)?,
        flags: read_u32_le(input, 12)?,
        texture_type: read_u32_le(input, 16)?,
        format: read_u32_le(input, 20)?,
        width: u16::from_le_bytes([input[24], input[25]]),
        height: u16::from_le_bytes([input[26], input[27]]),
        mip_count: input[28],
    })
}

fn validate_texture_info(info: TextureInfo) -> Result<(), AssetError> {
    if info.width == 0 || info.height == 0 {
        return Err(AssetError::InvalidFormat("zero-sized GXT texture".to_owned()));
    }
    // FUN_8102fe98 chooses 0x10-byte alignment only for palette_index == -1.
    // Editable mode does not preserve the GXT palette sections, so accepting any
    // other value would make the engine interpret the rebuilt descriptor as a
    // paletted texture and require a different data/palette layout.
    if info.palette_index != u32::MAX {
        return Err(AssetError::InvalidFormat(format!(
            "GXT palette_index={:#010x} requires palette data; use --raw-only",
            info.palette_index
        )));
    }
    // The Vita GXT field is the number of mipmaps beyond the base level. The
    // serializer currently emits only the base image, so any non-zero count
    // would advertise data that is not present.
    if info.mip_count != 0 {
        return Err(AssetError::InvalidFormat(format!(
            "GXT mip_count={} requires a mip chain; use --raw-only",
            info.mip_count
        )));
    }
    match info.texture_type {
        TEXTURE_TYPE_LINEAR | TEXTURE_TYPE_SWIZZLED => {}
        TEXTURE_TYPE_CUBE
        | TEXTURE_TYPE_TILED
        | TEXTURE_TYPE_SWIZZLED_ARBITRARY
        | TEXTURE_TYPE_LINEAR_STRIDED
        | TEXTURE_TYPE_CUBE_ARBITRARY => {
            return Err(AssetError::InvalidFormat(format!(
                "GXT texture type {:#010x} is not supported in editable mode",
                info.texture_type
            )))
        }
        value => {
            return Err(AssetError::InvalidFormat(format!(
                "unknown GXT texture type {value:#010x}"
            )))
        }
    }
    match info.format & FORMAT_BASE_MASK {
        FORMAT_U8U8U8U8 => {
            if matches!(info.format & FORMAT_SWIZZLE_MASK, 0x0000 | 0x1000 | 0x2000 | 0x3000) {
                Ok(())
            } else {
                Err(AssetError::InvalidFormat(format!(
                    "unsupported GXT channel swizzle {:#06x}",
                    info.format & FORMAT_SWIZZLE_MASK
                )))
            }
        }
        FORMAT_BC1 | FORMAT_BC2 | FORMAT_BC3 => {
            if matches!(info.format & FORMAT_SWIZZLE_MASK, 0x0000 | 0x4000) {
                Ok(())
            } else {
                Err(AssetError::InvalidFormat(format!(
                    "unsupported BC texture swizzle {:#06x}",
                    info.format & FORMAT_SWIZZLE_MASK
                )))
            }
        },
        value => Err(AssetError::InvalidFormat(format!(
            "unsupported GXT base format {value:#010x}"
        ))),
    }
}

fn infer_storage_extent(info: TextureInfo) -> Result<(u16, u16), AssetError> {
    let base = info.format & FORMAT_BASE_MASK;
    let unit_size = match base {
        FORMAT_U8U8U8U8 => 4usize,
        FORMAT_BC1 => 8usize,
        FORMAT_BC2 | FORMAT_BC3 => 16usize,
        _ => unreachable!(),
    };
    let data_size = usize::try_from(info.data_size)
        .map_err(|_| AssetError::InvalidFormat("GXT texture size overflows usize".to_owned()))?;
    let visible_width = usize::from(info.width);
    let visible_height = usize::from(info.height);

    let candidates = if base == FORMAT_U8U8U8U8 {
        vec![(visible_width, visible_height)]
    } else {
        let block_width = visible_width.div_ceil(4) * 4;
        let block_height = visible_height.div_ceil(4) * 4;
        let rounded_width = visible_width.next_power_of_two().max(4);
        let rounded_height = visible_height.next_power_of_two().max(4);
        let mut values = vec![(block_width, block_height)];
        if (rounded_width, rounded_height) != (block_width, block_height) {
            values.push((rounded_width, rounded_height));
        }
        values
    };

    for (width, height) in candidates {
        let grid_width = if base == FORMAT_U8U8U8U8 { width } else { width / 4 };
        let grid_height = if base == FORMAT_U8U8U8U8 { height } else { height / 4 };
        let expected = grid_width
            .checked_mul(grid_height)
            .and_then(|count| count.checked_mul(unit_size))
            .ok_or_else(|| AssetError::InvalidFormat("GXT storage extent overflow".to_owned()))?;
        if expected == data_size {
            let width = u16::try_from(width)
                .map_err(|_| AssetError::InvalidFormat("GXT storage width exceeds u16".to_owned()))?;
            let height = u16::try_from(height)
                .map_err(|_| AssetError::InvalidFormat("GXT storage height exceeds u16".to_owned()))?;
            if let Some(message) = storage_extent_violation(info, width, height) {
                return Err(AssetError::InvalidFormat(message.to_owned()));
            }
            return Ok((width, height));
        }
    }

    Err(AssetError::InvalidFormat(format!(
        "GXT data_size {} does not match the supported physical extents for {}x{}",
        info.data_size, info.width, info.height
    )))
}

fn storage_extent_violation(
    info: TextureInfo,
    storage_width: u16,
    storage_height: u16,
) -> Option<&'static str> {
    if storage_width < info.width || storage_height < info.height {
        return Some("GXT storage extent is smaller than the visible texture");
    }
    let base = info.format & FORMAT_BASE_MASK;
    if base == FORMAT_U8U8U8U8
        && (storage_width != info.width || storage_height != info.height)
    {
        return Some("uncompressed GXT storage extent must match the visible size");
    }
    if base != FORMAT_U8U8U8U8
        && (storage_width % 4 != 0 || storage_height % 4 != 0)
    {
        return Some("BC-compressed GXT storage extent must be block aligned");
    }
    let grid_width = if base == FORMAT_U8U8U8U8 {
        usize::from(storage_width)
    } else {
        usize::from(storage_width) / 4
    };
    let grid_height = if base == FORMAT_U8U8U8U8 {
        usize::from(storage_height)
    } else {
        usize::from(storage_height) / 4
    };
    if info.texture_type == TEXTURE_TYPE_SWIZZLED
        && (!grid_width.is_power_of_two() || !grid_height.is_power_of_two())
    {
        return Some(
            "regular swizzled GXT storage width and height must both be powers of two",
        );
    }
    None
}

fn decode_texture(
    input: &[u8],
    info: TextureInfo,
    storage_width: u16,
    storage_height: u16,
) -> Result<RgbaImage, AssetError> {
    let width = usize::from(storage_width);
    let height = usize::from(storage_height);
    let base = info.format & FORMAT_BASE_MASK;
    let (grid_width, grid_height, unit_size) = match base {
        FORMAT_U8U8U8U8 => (width, height, 4usize),
        FORMAT_BC1 => (width.div_ceil(4), height.div_ceil(4), 8usize),
        FORMAT_BC2 | FORMAT_BC3 => (width.div_ceil(4), height.div_ceil(4), 16usize),
        _ => unreachable!(),
    };
    let required = grid_width
        .checked_mul(grid_height)
        .and_then(|count| count.checked_mul(unit_size))
        .ok_or_else(|| AssetError::InvalidFormat("GXT texture size overflow".to_owned()))?;
    if input.len() < required {
        return Err(AssetError::InvalidFormat(format!(
            "GXT texture is truncated: needs {required}, has {}",
            input.len()
        )));
    }

    let mut output = RgbaImage::new(u32::from(info.width), u32::from(info.height));
    for y in 0..grid_height {
        for x in 0..grid_width {
            let unit_index = physical_index(x, y, grid_width, grid_height, info.texture_type)?;
            let start = unit_index
                .checked_mul(unit_size)
                .ok_or_else(|| AssetError::InvalidFormat("GXT unit offset overflow".to_owned()))?;
            let unit = &input[start..start + unit_size];
            match base {
                FORMAT_U8U8U8U8 => {
                    let rgba = decode_uncompressed_pixel(unit, info.format);
                    output.put_pixel(x as u32, y as u32, Rgba(rgba));
                }
                FORMAT_BC1 => write_block(&mut output, x, y, &decode_bc1(unit)),
                FORMAT_BC2 => write_block(&mut output, x, y, &decode_bc2(unit)),
                FORMAT_BC3 => write_block(&mut output, x, y, &decode_bc3(unit)),
                _ => unreachable!(),
            }
        }
    }
    if base != FORMAT_U8U8U8U8 && info.format & FORMAT_SWIZZLE_MASK == 0x4000 {
        for pixel in output.pixels_mut() {
            pixel.0[3] = 255;
        }
    }
    Ok(output)
}

fn encode_texture(
    image: &RgbaImage,
    info: TextureInfo,
    storage_width: u16,
    storage_height: u16,
) -> Result<Vec<u8>, AssetError> {
    let width = usize::from(storage_width);
    let height = usize::from(storage_height);
    let base = info.format & FORMAT_BASE_MASK;
    let (grid_width, grid_height, unit_size) = match base {
        FORMAT_U8U8U8U8 => (width, height, 4usize),
        FORMAT_BC1 => (width.div_ceil(4), height.div_ceil(4), 8usize),
        FORMAT_BC2 | FORMAT_BC3 => (width.div_ceil(4), height.div_ceil(4), 16usize),
        _ => unreachable!(),
    };
    let output_size = grid_width
        .checked_mul(grid_height)
        .and_then(|count| count.checked_mul(unit_size))
        .ok_or_else(|| AssetError::InvalidProject("GXT output size overflow".to_owned()))?;
    let mut output = vec![0u8; output_size];
    for y in 0..grid_height {
        for x in 0..grid_width {
            let unit_index = physical_index(x, y, grid_width, grid_height, info.texture_type)?;
            let start = unit_index
                .checked_mul(unit_size)
                .ok_or_else(|| AssetError::InvalidProject("GXT unit offset overflow".to_owned()))?;
            match base {
                FORMAT_U8U8U8U8 => {
                    let source_x = x.min(image.width() as usize - 1) as u32;
                    let source_y = y.min(image.height() as usize - 1) as u32;
                    let pixel = image.get_pixel(source_x, source_y).0;
                    output[start..start + 4]
                        .copy_from_slice(&encode_uncompressed_pixel(pixel, info.format));
                }
                FORMAT_BC1 => {
                    let block = read_block(image, x, y);
                    output[start..start + 8].copy_from_slice(&encode_bc1(&block));
                }
                FORMAT_BC2 => {
                    let block = read_block(image, x, y);
                    output[start..start + 16].copy_from_slice(&encode_bc2(&block));
                }
                FORMAT_BC3 => {
                    let block = read_block(image, x, y);
                    output[start..start + 16].copy_from_slice(&encode_bc3(&block));
                }
                _ => unreachable!(),
            }
        }
    }
    Ok(output)
}


fn encode_texture_preserving_source(
    image: &RgbaImage,
    info: TextureInfo,
    storage_width: u16,
    storage_height: u16,
    source: &[u8],
) -> Result<Vec<u8>, AssetError> {
    let width = usize::from(storage_width);
    let height = usize::from(storage_height);
    let base = info.format & FORMAT_BASE_MASK;
    let (grid_width, grid_height, unit_size) = match base {
        FORMAT_U8U8U8U8 => (width, height, 4usize),
        FORMAT_BC1 => (width.div_ceil(4), height.div_ceil(4), 8usize),
        FORMAT_BC2 | FORMAT_BC3 => (width.div_ceil(4), height.div_ceil(4), 16usize),
        _ => unreachable!(),
    };
    let output_size = grid_width
        .checked_mul(grid_height)
        .and_then(|count| count.checked_mul(unit_size))
        .ok_or_else(|| AssetError::InvalidProject("GXT output size overflow".to_owned()))?;
    if source.len() != output_size {
        return Err(AssetError::InvalidProject(format!(
            "source texture has {} bytes, physical layout requires {output_size}",
            source.len()
        )));
    }

    let mut output = source.to_vec();
    for y in 0..grid_height {
        for x in 0..grid_width {
            let unit_index = physical_index(x, y, grid_width, grid_height, info.texture_type)
                .map_err(|error| AssetError::InvalidProject(error.to_string()))?;
            let start = unit_index
                .checked_mul(unit_size)
                .ok_or_else(|| AssetError::InvalidProject("GXT unit offset overflow".to_owned()))?;
            let source_unit = &source[start..start + unit_size];
            match base {
                FORMAT_U8U8U8U8 => {
                    let target = image.get_pixel(x as u32, y as u32).0;
                    let observed = decode_uncompressed_pixel(source_unit, info.format);
                    if target != observed {
                        output[start..start + 4]
                            .copy_from_slice(&encode_uncompressed_pixel(target, info.format));
                    }
                }
                FORMAT_BC1 | FORMAT_BC2 | FORMAT_BC3 => {
                    let mut observed = match base {
                        FORMAT_BC1 => decode_bc1(source_unit),
                        FORMAT_BC2 => decode_bc2(source_unit),
                        FORMAT_BC3 => decode_bc3(source_unit),
                        _ => unreachable!(),
                    };
                    if info.format & FORMAT_SWIZZLE_MASK == 0x4000 {
                        for pixel in &mut observed {
                            pixel[3] = 255;
                        }
                    }
                    let (target, changed) = read_block_preserving_source(image, x, y, observed, info.format);
                    if !changed {
                        continue;
                    }
                    let encoded = match base {
                        FORMAT_BC1 => encode_bc1_seeded(&target, Some(source_unit)).to_vec(),
                        FORMAT_BC2 => encode_bc2_seeded(&target, Some(source_unit)).to_vec(),
                        FORMAT_BC3 => encode_bc3_seeded(&target, Some(source_unit)).to_vec(),
                        _ => unreachable!(),
                    };
                    let mut rebuilt = match base {
                        FORMAT_BC1 => decode_bc1(&encoded),
                        FORMAT_BC2 => decode_bc2(&encoded),
                        FORMAT_BC3 => decode_bc3(&encoded),
                        _ => unreachable!(),
                    };
                    if info.format & FORMAT_SWIZZLE_MASK == 0x4000 {
                        for pixel in &mut rebuilt {
                            pixel[3] = 255;
                        }
                    }
                    let source_error = block_visual_error(&observed, &target, info.format);
                    let rebuilt_error = block_visual_error(&rebuilt, &target, info.format);
                    if rebuilt == observed {
                        return Err(AssetError::InvalidProject(format!(
                            "BC block ({x},{y}) edit is below the format's representable resolution (source error {source_error}, encoded error {rebuilt_error})"
                        )));
                    }
                    output[start..start + unit_size].copy_from_slice(&encoded);
                }
                _ => unreachable!(),
            }
        }
    }
    Ok(output)
}

fn block_visual_error(
    observed: &[[u8; 4]; 16],
    target: &[[u8; 4]; 16],
    format: u32,
) -> u64 {
    let ignore_alpha = format & FORMAT_SWIZZLE_MASK == 0x4000;
    observed
        .iter()
        .zip(target.iter())
        .fold(0u64, |sum, (left, right)| {
            let channels = if ignore_alpha { 3 } else { 4 };
            (0..channels).fold(sum, |sum, channel| {
                let delta = i64::from(left[channel]) - i64::from(right[channel]);
                sum.saturating_add((delta * delta) as u64)
            })
        })
}

fn read_block_preserving_source(
    image: &RgbaImage,
    block_x: usize,
    block_y: usize,
    mut source_pixels: [[u8; 4]; 16],
    format: u32,
) -> ([[u8; 4]; 16], bool) {
    let mut changed = false;
    for py in 0..4 {
        for px in 0..4 {
            let x = block_x * 4 + px;
            let y = block_y * 4 + py;
            if x >= image.width() as usize || y >= image.height() as usize {
                continue;
            }
            let mut target = image.get_pixel(x as u32, y as u32).0;
            if format & FORMAT_SWIZZLE_MASK == 0x4000 {
                target[3] = 255;
            }
            let index = py * 4 + px;
            changed |= source_pixels[index] != target;
            source_pixels[index] = target;
        }
    }
    (source_pixels, changed)
}

fn physical_index(
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    texture_type: u32,
) -> Result<usize, AssetError> {
    match texture_type {
        TEXTURE_TYPE_LINEAR => y
            .checked_mul(width)
            .and_then(|row| row.checked_add(x))
            .ok_or_else(|| AssetError::InvalidFormat("GXT linear index overflow".to_owned())),
        TEXTURE_TYPE_SWIZZLED => twiddled_index(x, y, width, height),
        _ => Err(AssetError::InvalidFormat("unsupported GXT texture layout".to_owned())),
    }
}

fn twiddled_index(x: usize, y: usize, width: usize, height: usize) -> Result<usize, AssetError> {
    if width == 0
        || height == 0
        || !width.is_power_of_two()
        || !height.is_power_of_two()
    {
        return Err(AssetError::InvalidFormat(
            "regular swizzled GXT requires power-of-two width and height".to_owned(),
        ));
    }
    let minimum = width.min(height);
    let square = minimum
        .checked_mul(minimum)
        .ok_or_else(|| AssetError::InvalidFormat("GXT swizzle size overflow".to_owned()))?;
    let local_x = x & (minimum - 1);
    let local_y = y & (minimum - 1);
    // Vita GXT interleaves the logical Y bit first, then X. This is the
    // inverse of the common x-first Morton helper used by many desktop tools.
    let local = morton2(local_y, local_x);
    let strip = if width >= height {
        x / minimum
    } else {
        y / minimum
    };
    let index = strip
        .checked_mul(square)
        .and_then(|base| base.checked_add(local))
        .ok_or_else(|| AssetError::InvalidFormat("GXT swizzle index overflow".to_owned()))?;
    let total = width
        .checked_mul(height)
        .ok_or_else(|| AssetError::InvalidFormat("GXT swizzle extent overflow".to_owned()))?;
    if index >= total {
        return Err(AssetError::InvalidFormat("invalid GXT swizzle index".to_owned()));
    }
    Ok(index)
}

fn morton2(x: usize, y: usize) -> usize {
    let mut result = 0usize;
    let bits = usize::BITS as usize / 2;
    for bit in 0..bits {
        result |= ((x >> bit) & 1) << (bit * 2);
        result |= ((y >> bit) & 1) << (bit * 2 + 1);
    }
    result
}

pub(crate) fn decode_uncompressed_pixel(input: &[u8], format: u32) -> [u8; 4] {
    match format & FORMAT_SWIZZLE_MASK {
        0x0000 => [input[0], input[1], input[2], input[3]],
        0x1000 => [input[2], input[1], input[0], input[3]],
        0x2000 => [input[3], input[2], input[1], input[0]],
        0x3000 => [input[1], input[2], input[3], input[0]],
        _ => [input[0], input[1], input[2], input[3]],
    }
}

pub(crate) fn encode_uncompressed_pixel(rgba: [u8; 4], format: u32) -> [u8; 4] {
    match format & FORMAT_SWIZZLE_MASK {
        0x0000 => rgba,
        0x1000 => [rgba[2], rgba[1], rgba[0], rgba[3]],
        0x2000 => [rgba[3], rgba[2], rgba[1], rgba[0]],
        0x3000 => [rgba[3], rgba[0], rgba[1], rgba[2]],
        _ => rgba,
    }
}

fn write_block(image: &mut RgbaImage, block_x: usize, block_y: usize, pixels: &[[u8; 4]; 16]) {
    for py in 0..4 {
        for px in 0..4 {
            let x = block_x * 4 + px;
            let y = block_y * 4 + py;
            if x < image.width() as usize && y < image.height() as usize {
                image.put_pixel(x as u32, y as u32, Rgba(pixels[py * 4 + px]));
            }
        }
    }
}

fn read_block(image: &RgbaImage, block_x: usize, block_y: usize) -> [[u8; 4]; 16] {
    let mut result = [[0u8; 4]; 16];
    for py in 0..4 {
        for px in 0..4 {
            let x = (block_x * 4 + px).min(image.width() as usize - 1);
            let y = (block_y * 4 + py).min(image.height() as usize - 1);
            result[py * 4 + px] = image.get_pixel(x as u32, y as u32).0;
        }
    }
    result
}

fn decode_bc1(input: &[u8]) -> [[u8; 4]; 16] {
    let c0 = u16::from_le_bytes([input[0], input[1]]);
    let c1 = u16::from_le_bytes([input[2], input[3]]);
    let palette = bc1_palette(c0, c1, true);
    let indices = u32::from_le_bytes([input[4], input[5], input[6], input[7]]);
    let mut output = [[0u8; 4]; 16];
    for (index, pixel) in output.iter_mut().enumerate() {
        *pixel = palette[((indices >> (index * 2)) & 3) as usize];
    }
    output
}

fn decode_bc2(input: &[u8]) -> [[u8; 4]; 16] {
    let mut output = decode_bc1_opaque(&input[8..16]);
    let alpha = u64::from_le_bytes(input[0..8].try_into().unwrap());
    for (index, pixel) in output.iter_mut().enumerate() {
        let value = ((alpha >> (index * 4)) & 0xf) as u8;
        pixel[3] = value * 17;
    }
    output
}

fn decode_bc3(input: &[u8]) -> [[u8; 4]; 16] {
    let mut output = decode_bc1_opaque(&input[8..16]);
    let palette = alpha_palette(input[0], input[1]);
    let mut bits = 0u64;
    for (index, byte) in input[2..8].iter().enumerate() {
        bits |= u64::from(*byte) << (index * 8);
    }
    for (index, pixel) in output.iter_mut().enumerate() {
        pixel[3] = palette[((bits >> (index * 3)) & 7) as usize];
    }
    output
}

fn decode_bc1_opaque(input: &[u8]) -> [[u8; 4]; 16] {
    let c0 = u16::from_le_bytes([input[0], input[1]]);
    let c1 = u16::from_le_bytes([input[2], input[3]]);
    let palette = bc1_palette(c0, c1, false);
    let indices = u32::from_le_bytes([input[4], input[5], input[6], input[7]]);
    let mut output = [[0u8; 4]; 16];
    for (index, pixel) in output.iter_mut().enumerate() {
        *pixel = palette[((indices >> (index * 2)) & 3) as usize];
    }
    output
}

fn encode_bc1(pixels: &[[u8; 4]; 16]) -> [u8; 8] {
    encode_bc1_seeded(pixels, None)
}

fn encode_bc1_seeded(pixels: &[[u8; 4]; 16], source: Option<&[u8]>) -> [u8; 8] {
    let transparent = pixels.iter().any(|pixel| pixel[3] < 128);
    let opaque = pixels
        .iter()
        .copied()
        .filter(|pixel| pixel[3] >= 128)
        .collect::<Vec<_>>();
    if transparent && opaque.is_empty() {
        let mut output = [0u8; 8];
        output[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        return output;
    }

    let color_pixels = if transparent { opaque.as_slice() } else { &pixels[..] };
    let source_endpoints = source.and_then(|bytes| {
        (bytes.len() >= 4).then(|| {
            (
                u16::from_le_bytes([bytes[0], bytes[1]]),
                u16::from_le_bytes([bytes[2], bytes[3]]),
            )
        })
    });
    let (c0, c1) = optimize_color_endpoints(color_pixels, transparent, source_endpoints);
    let palette = bc1_palette(c0, c1, transparent);
    let limit = if transparent { 3 } else { 4 };
    let mut indices = 0u32;
    for (index, pixel) in pixels.iter().enumerate() {
        let palette_index = if transparent && pixel[3] < 128 {
            3
        } else {
            nearest_color_limit(*pixel, &palette, limit)
        };
        indices |= (palette_index as u32) << (index * 2);
    }
    let mut output = [0u8; 8];
    output[0..2].copy_from_slice(&c0.to_le_bytes());
    output[2..4].copy_from_slice(&c1.to_le_bytes());
    output[4..8].copy_from_slice(&indices.to_le_bytes());
    output
}

fn encode_bc2(pixels: &[[u8; 4]; 16]) -> [u8; 16] {
    encode_bc2_seeded(pixels, None)
}

fn encode_bc2_seeded(pixels: &[[u8; 4]; 16], source: Option<&[u8]>) -> [u8; 16] {
    let mut output = [0u8; 16];
    let mut alpha = 0u64;
    for (index, pixel) in pixels.iter().enumerate() {
        alpha |= u64::from((u16::from(pixel[3]) + 8) / 17) << (index * 4);
    }
    output[0..8].copy_from_slice(&alpha.to_le_bytes());
    let source_color = source.and_then(|bytes| bytes.get(8..16));
    output[8..16].copy_from_slice(&encode_color_block_seeded(pixels, source_color));
    output
}

fn encode_bc3(pixels: &[[u8; 4]; 16]) -> [u8; 16] {
    encode_bc3_seeded(pixels, None)
}

fn encode_bc3_seeded(pixels: &[[u8; 4]; 16], source: Option<&[u8]>) -> [u8; 16] {
    let mut output = [0u8; 16];
    let source_alpha = source.and_then(|bytes| (bytes.len() >= 2).then(|| (bytes[0], bytes[1])));
    let (alpha0, alpha1) = optimize_alpha_endpoints(pixels, source_alpha);
    output[0] = alpha0;
    output[1] = alpha1;
    let palette = alpha_palette(alpha0, alpha1);
    let mut bits = 0u64;
    for (index, pixel) in pixels.iter().enumerate() {
        let nearest = nearest_alpha(pixel[3], &palette);
        bits |= (nearest as u64) << (index * 3);
    }
    for index in 0..6 {
        output[2 + index] = ((bits >> (index * 8)) & 0xff) as u8;
    }
    let source_color = source.and_then(|bytes| bytes.get(8..16));
    output[8..16].copy_from_slice(&encode_color_block_seeded(pixels, source_color));
    output
}

fn encode_color_block(pixels: &[[u8; 4]; 16]) -> [u8; 8] {
    encode_color_block_seeded(pixels, None)
}

fn encode_color_block_seeded(pixels: &[[u8; 4]; 16], source: Option<&[u8]>) -> [u8; 8] {
    let source_endpoints = source.and_then(|bytes| {
        (bytes.len() >= 4).then(|| {
            (
                u16::from_le_bytes([bytes[0], bytes[1]]),
                u16::from_le_bytes([bytes[2], bytes[3]]),
            )
        })
    });
    let (c0, c1) = optimize_color_endpoints(pixels, false, source_endpoints);
    let palette = bc1_palette(c0, c1, false);
    let mut indices = 0u32;
    for (index, pixel) in pixels.iter().enumerate() {
        indices |= (nearest_color(*pixel, &palette) as u32) << (index * 2);
    }
    let mut output = [0u8; 8];
    output[0..2].copy_from_slice(&c0.to_le_bytes());
    output[2..4].copy_from_slice(&c1.to_le_bytes());
    output[4..8].copy_from_slice(&indices.to_le_bytes());
    output
}

fn optimize_color_endpoints(
    pixels: &[[u8; 4]],
    transparent_mode: bool,
    source: Option<(u16, u16)>,
) -> (u16, u16) {
    let (farthest_a, farthest_b) = farthest_color_pair(pixels);
    let (darkest, brightest) = luminance_extremes(pixels);
    let (minimum, maximum) = channel_extremes(pixels);
    let mut seed_pairs = vec![
        (rgb_to_565(farthest_a), rgb_to_565(farthest_b)),
        (rgb_to_565(darkest), rgb_to_565(brightest)),
        (rgb_to_565(minimum), rgb_to_565(maximum)),
    ];
    if let Some(pair) = source {
        seed_pairs.push(pair);
    }
    seed_pairs.sort_unstable();
    seed_pairs.dedup();

    let mut best = if transparent_mode { (0u16, 0u16) } else { (1u16, 0u16) };
    let mut best_error = u64::MAX;
    for (seed_a, seed_b) in seed_pairs {
        let mut candidates_a = Vec::new();
        let mut candidates_b = Vec::new();
        push_565_neighborhood(&mut candidates_a, seed_a);
        push_565_neighborhood(&mut candidates_b, seed_b);
        candidates_a.sort_unstable();
        candidates_a.dedup();
        candidates_b.sort_unstable();
        candidates_b.dedup();
        for &a in &candidates_a {
            for &b in &candidates_b {
                let (c0, c1) = if transparent_mode {
                    if a <= b { (a, b) } else { (b, a) }
                } else if a > b {
                    (a, b)
                } else if b > a {
                    (b, a)
                } else {
                    continue;
                };
                let palette = bc1_palette(c0, c1, transparent_mode);
                let limit = if transparent_mode { 3 } else { 4 };
                let error = pixels.iter().fold(0u64, |sum, pixel| {
                    let index = nearest_color_limit(*pixel, &palette, limit);
                    sum.saturating_add(u64::from(color_distance(*pixel, palette[index])))
                });
                if error < best_error {
                    best_error = error;
                    best = (c0, c1);
                }
            }
        }
    }
    best
}

fn farthest_color_pair(pixels: &[[u8; 4]]) -> ([u8; 4], [u8; 4]) {
    let fallback = pixels.first().copied().unwrap_or([0, 0, 0, 255]);
    let mut best = (fallback, fallback);
    let mut best_distance = 0u64;
    for &a in pixels {
        for &b in pixels {
            let distance = u64::from(color_distance(a, b));
            if distance > best_distance {
                best_distance = distance;
                best = (a, b);
            }
        }
    }
    best
}

fn luminance_extremes(pixels: &[[u8; 4]]) -> ([u8; 4], [u8; 4]) {
    let fallback = pixels.first().copied().unwrap_or([0, 0, 0, 255]);
    let darkest = pixels
        .iter()
        .copied()
        .min_by_key(|pixel| 54u32 * u32::from(pixel[0]) + 183u32 * u32::from(pixel[1]) + 19u32 * u32::from(pixel[2]))
        .unwrap_or(fallback);
    let brightest = pixels
        .iter()
        .copied()
        .max_by_key(|pixel| 54u32 * u32::from(pixel[0]) + 183u32 * u32::from(pixel[1]) + 19u32 * u32::from(pixel[2]))
        .unwrap_or(fallback);
    (darkest, brightest)
}

fn channel_extremes(pixels: &[[u8; 4]]) -> ([u8; 4], [u8; 4]) {
    let mut minimum = [u8::MAX, u8::MAX, u8::MAX, 255];
    let mut maximum = [u8::MIN, u8::MIN, u8::MIN, 255];
    for pixel in pixels {
        for channel in 0..3 {
            minimum[channel] = minimum[channel].min(pixel[channel]);
            maximum[channel] = maximum[channel].max(pixel[channel]);
        }
    }
    (minimum, maximum)
}

fn push_565_neighborhood(output: &mut Vec<u16>, value: u16) {
    let r = ((value >> 11) & 0x1f) as i32;
    let g = ((value >> 5) & 0x3f) as i32;
    let b = (value & 0x1f) as i32;
    for (dr, dg, db) in [
        (0, 0, 0),
        (-1, 0, 0),
        (1, 0, 0),
        (0, -1, 0),
        (0, 1, 0),
        (0, 0, -1),
        (0, 0, 1),
    ] {
        let nr = (r + dr).clamp(0, 0x1f) as u16;
        let ng = (g + dg).clamp(0, 0x3f) as u16;
        let nb = (b + db).clamp(0, 0x1f) as u16;
        output.push((nr << 11) | (ng << 5) | nb);
    }
}

fn optimize_alpha_endpoints(
    pixels: &[[u8; 4]; 16],
    source: Option<(u8, u8)>,
) -> (u8, u8) {
    let minimum = pixels.iter().map(|pixel| pixel[3]).min().unwrap_or(0);
    let maximum = pixels.iter().map(|pixel| pixel[3]).max().unwrap_or(255);
    let mut seeds = vec![(maximum, minimum), (minimum, maximum), (255, 0), (0, 255)];
    if let Some(pair) = source {
        seeds.push(pair);
    }
    seeds.sort_unstable();
    seeds.dedup();

    let mut best = (255u8, 0u8);
    let mut best_error = u64::MAX;
    for (seed0, seed1) in seeds {
        let candidates0 = alpha_neighborhood(seed0);
        let candidates1 = alpha_neighborhood(seed1);
        for &a0 in &candidates0 {
            for &a1 in &candidates1 {
                let palette = alpha_palette(a0, a1);
                let error = pixels.iter().fold(0u64, |sum, pixel| {
                    let index = nearest_alpha(pixel[3], &palette);
                    let delta = i64::from(pixel[3]) - i64::from(palette[index]);
                    sum.saturating_add((delta * delta) as u64)
                });
                if error < best_error {
                    best_error = error;
                    best = (a0, a1);
                }
            }
        }
    }
    best
}

fn alpha_neighborhood(value: u8) -> Vec<u8> {
    let mut values = Vec::with_capacity(5);
    for delta in -2i16..=2 {
        values.push((i16::from(value) + delta).clamp(0, 255) as u8);
    }
    values.sort_unstable();
    values.dedup();
    values
}

fn bc1_palette(c0: u16, c1: u16, allow_transparency: bool) -> [[u8; 4]; 4] {
    let a = rgb_from_565(c0);
    let b = rgb_from_565(c1);
    let mut output = [a, b, [0; 4], [0; 4]];
    if c0 > c1 || !allow_transparency {
        output[2] = interpolate(a, b, 2, 1, 3);
        output[3] = interpolate(a, b, 1, 2, 3);
    } else {
        output[2] = interpolate(a, b, 1, 1, 2);
        output[3] = [0, 0, 0, 0];
    }
    output
}

fn alpha_palette(a0: u8, a1: u8) -> [u8; 8] {
    let mut output = [0u8; 8];
    output[0] = a0;
    output[1] = a1;
    if a0 > a1 {
        for index in 1..=6 {
            output[index + 1] = (((7 - index) as u16 * u16::from(a0)
                + index as u16 * u16::from(a1)) / 7) as u8;
        }
    } else {
        for index in 1..=4 {
            output[index + 1] = (((5 - index) as u16 * u16::from(a0)
                + index as u16 * u16::from(a1)) / 5) as u8;
        }
        output[6] = 0;
        output[7] = 255;
    }
    output
}

fn interpolate(a: [u8; 4], b: [u8; 4], aw: u16, bw: u16, divisor: u16) -> [u8; 4] {
    let mut output = [0u8; 4];
    for channel in 0..4 {
        output[channel] = ((u16::from(a[channel]) * aw + u16::from(b[channel]) * bw) / divisor) as u8;
    }
    output
}

fn rgb_from_565(value: u16) -> [u8; 4] {
    let r = ((value >> 11) & 0x1f) as u8;
    let g = ((value >> 5) & 0x3f) as u8;
    let b = (value & 0x1f) as u8;
    [
        (r << 3) | (r >> 2),
        (g << 2) | (g >> 4),
        (b << 3) | (b >> 2),
        255,
    ]
}

fn rgb_to_565(pixel: [u8; 4]) -> u16 {
    (u16::from(pixel[0] >> 3) << 11)
        | (u16::from(pixel[1] >> 2) << 5)
        | u16::from(pixel[2] >> 3)
}

fn nearest_color(pixel: [u8; 4], palette: &[[u8; 4]; 4]) -> usize {
    nearest_color_limit(pixel, palette, palette.len())
}

fn nearest_color_limit(pixel: [u8; 4], palette: &[[u8; 4]; 4], limit: usize) -> usize {
    palette
        .iter()
        .take(limit)
        .enumerate()
        .min_by_key(|(_, candidate)| color_distance(pixel, **candidate))
        .map(|(index, _)| index)
        .unwrap_or(0)
}

fn nearest_alpha(alpha: u8, palette: &[u8; 8]) -> usize {
    palette
        .iter()
        .enumerate()
        .min_by_key(|(_, candidate)| alpha.abs_diff(**candidate))
        .map(|(index, _)| index)
        .unwrap_or(0)
}

fn color_distance(a: [u8; 4], b: [u8; 4]) -> u32 {
    let dr = i32::from(a[0]) - i32::from(b[0]);
    let dg = i32::from(a[1]) - i32::from(b[1]);
    let db = i32::from(a[2]) - i32::from(b[2]);
    (dr * dr + dg * dg + db * db) as u32
}

fn read_u32_le(input: &[u8], offset: usize) -> Result<u32, AssetError> {
    let bytes = input
        .get(offset..offset + 4)
        .ok_or_else(|| AssetError::InvalidFormat("truncated little-endian u32".to_owned()))?;
    Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
}

fn push_u32_le(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn checked_align_up(value: usize, alignment: usize) -> Option<usize> {
    debug_assert!(alignment.is_power_of_two());
    value
        .checked_add(alignment.checked_sub(1)?)
        .map(|aligned| aligned & !(alignment - 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bc1_round_trip_has_expected_size() {
        let pixels = [[128, 64, 32, 255]; 16];
        let encoded = encode_bc1(&pixels);
        let decoded = decode_bc1(&encoded);
        assert_eq!(encoded.len(), 8);
        assert_eq!(decoded.len(), 16);
    }

    #[test]
    fn bc1_transparency_uses_the_three_color_mode() {
        let mut pixels = [[128, 64, 32, 255]; 16];
        pixels[5][3] = 0;
        let encoded = encode_bc1(&pixels);
        let c0 = u16::from_le_bytes([encoded[0], encoded[1]]);
        let c1 = u16::from_le_bytes([encoded[2], encoded[3]]);
        assert!(c0 <= c1);
        assert_eq!(decode_bc1(&encoded)[5][3], 0);
    }

    #[test]
    fn source_aware_bc_keeps_unchanged_blocks_byte_exact() {
        let layout = EngineTextureLayout {
            palette_index: u32::MAX,
            flags: 0,
            texture_type: TEXTURE_TYPE_LINEAR,
            format: FORMAT_BC1,
            width: 8,
            height: 4,
            mip_count: 0,
        };
        let mut image = RgbaImage::new(8, 4);
        for y in 0..4 {
            for x in 0..8 {
                image.put_pixel(x, y, Rgba(if x < 4 { [20, 40, 60, 255] } else { [180, 30, 10, 255] }));
            }
        }
        let source = encode_engine_texture(&image, layout, 16).unwrap();
        assert_eq!(
            encode_engine_texture_preserving_source(&image, layout, 16, &source).unwrap(),
            source
        );

        let mut edited = image.clone();
        edited.put_pixel(6, 2, Rgba([0, 255, 0, 255]));
        let rebuilt = encode_engine_texture_preserving_source(&edited, layout, 16, &source).unwrap();
        assert_eq!(&rebuilt[..8], &source[..8]);
        assert_ne!(&rebuilt[8..], &source[8..]);
    }

    #[test]
    fn rectangular_twiddle_stays_in_range() {
        for y in 0..8 {
            for x in 0..16 {
                assert!(twiddled_index(x, y, 16, 8).unwrap() < 128);
            }
        }
    }

    #[test]
    fn vita_twiddle_interleaves_y_before_x() {
        assert_eq!(twiddled_index(0, 0, 2, 2).unwrap(), 0);
        assert_eq!(twiddled_index(0, 1, 2, 2).unwrap(), 1);
        assert_eq!(twiddled_index(1, 0, 2, 2).unwrap(), 2);
        assert_eq!(twiddled_index(1, 1, 2, 2).unwrap(), 3);
    }

    #[test]
    fn uncompressed_channel_swizzles_are_inverses() {
        let rgba = [1, 2, 3, 4];
        for swizzle in [0x0000, 0x1000, 0x2000, 0x3000] {
            let format = FORMAT_U8U8U8U8 | swizzle;
            let encoded = encode_uncompressed_pixel(rgba, format);
            assert_eq!(decode_uncompressed_pixel(&encoded, format), rgba);
        }
    }

    #[test]
    fn editable_gxt_rejects_palette_and_mip_descriptors() {
        let base = TextureInfo {
            data_offset: 0,
            data_size: 64,
            palette_index: u32::MAX,
            flags: 0,
            texture_type: TEXTURE_TYPE_LINEAR,
            format: FORMAT_U8U8U8U8,
            width: 4,
            height: 4,
            mip_count: 0,
        };
        assert!(validate_texture_info(base).is_ok());
        assert!(validate_texture_info(TextureInfo {
            palette_index: 0,
            ..base
        })
        .is_err());
        assert!(validate_texture_info(TextureInfo {
            mip_count: 1,
            ..base
        })
        .is_err());
    }

    #[test]
    fn swizzled_rgba_texture_round_trip_preserves_pixels() {
        let info = TextureInfo {
            data_offset: 0,
            data_size: 0,
            palette_index: u32::MAX,
            flags: 0,
            texture_type: TEXTURE_TYPE_SWIZZLED,
            format: FORMAT_U8U8U8U8,
            width: 4,
            height: 4,
            mip_count: 0,
        };
        let mut image = RgbaImage::new(4, 4);
        for y in 0..4 {
            for x in 0..4 {
                image.put_pixel(x, y, Rgba([x as u8, y as u8, (x + y) as u8, 255]));
            }
        }
        let encoded = encode_texture(&image, info, 4, 4).unwrap();
        let decoded = decode_texture(&encoded, info, 4, 4).unwrap();
        assert_eq!(decoded, image);
    }
}
