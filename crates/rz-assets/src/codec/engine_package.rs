use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::Path;

use flate2::read::GzDecoder;
use flate2::{Compression, GzBuilder};
use image::{Rgba, RgbaImage};
use serde::{Deserialize, Serialize};

use crate::codec::gxt::{
    decode_engine_texture, decode_uncompressed_pixel, encode_engine_texture_preserving_source,
    encode_uncompressed_pixel, EngineTextureLayout,
};
use crate::error::AssetError;
use crate::manifest::AssetKind;

const PACKAGE_HEADER_SIZE: usize = 0x40;
const TEXTURE_DESCRIPTOR_SIZE: usize = 0x30;
const SECONDARY_TABLE_OFFSET: usize = 0x1400;
const DOCUMENT_VERSION: u32 = 1;
const MAX_CHUNKS: usize = 0xff;
const SCRATCH_CAPACITY: usize = 0x1800000;
pub(crate) const ENGINE_INTEGRITY_FOOTER_SIZE: usize = 0x10;
const ENGINE_INTEGRITY_SEED: u64 = 0x1111_1111_1111_1111;

const TEXTURE_TYPE_LINEAR: u32 = 0x6000_0000;
const FORMAT_BASE_MASK: u32 = 0xff00_0000;
const FORMAT_SWIZZLE_MASK: u32 = 0x0000_f000;
const FORMAT_P4: u32 = 0x9400_0000;
const FORMAT_P8: u32 = 0x9500_0000;
const FORMAT_BC1: u32 = 0x8500_0000;
const FORMAT_BC2: u32 = 0x8600_0000;
const FORMAT_BC3: u32 = 0x8700_0000;
const FORMAT_RUNTIME_RGBA8: u32 = 0x0c00_1000;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum EnginePackageProfile {
    AddPt,
    Bk,
    Bsf,
    Pt,
}

impl EnginePackageProfile {
    fn uses_bundle(self) -> bool {
        matches!(self, Self::AddPt | Self::Pt)
    }

    fn creates_texture_from_descriptor(self) -> bool {
        matches!(self, Self::AddPt | Self::Pt)
    }

    fn uses_metadata_records(self) -> bool {
        self.creates_texture_from_descriptor()
    }

    fn table_offset(self) -> usize {
        match self {
            Self::Bk | Self::Bsf => SECONDARY_TABLE_OFFSET,
            Self::AddPt | Self::Pt => 0,
        }
    }

    fn maximum_decode_size(self, entry_id: u32) -> Result<usize, AssetError> {
        match self {
            Self::AddPt | Self::Pt => Ok(SCRATCH_CAPACITY),
            Self::Bk => {
                if entry_id >= 0x146 {
                    return Err(AssetError::InvalidFormat(format!(
                        "bk.cpk entry ID {entry_id} is outside the engine range 0..325"
                    )));
                }
                Ok(0x220000)
            }
            Self::Bsf => {
                if entry_id < 0x9a {
                    Ok(0x320000)
                } else if entry_id < 0x126 {
                    Ok(0x300000)
                } else {
                    Err(AssetError::InvalidFormat(format!(
                        "bsf.cpk entry ID {entry_id} is outside the engine range 0..293"
                    )))
                }
            }
        }
    }

    fn runtime_surface(
        self,
        entry_id: u32,
        descriptor_height: Option<u16>,
    ) -> Result<Option<RuntimeSurface>, AssetError> {
        let (width, capacity_height, editable_height, descriptor_height) = match self {
            // FUN_81053cda copies the complete executable-created 1024x544
            // surface. Its call to FUN_81053694 passes a null handle output, so
            // package+0x04 is not dereferenced as a texture descriptor on this
            // path. The later copy uses the executable-created runtime handle.
            Self::Bk => (1024u16, 544u16, 544u16, 544u16),
            // FUN_8101fea6 does dereference package+package[+0x04] and passes
            // descriptor+0x2a as the row count for BSF.
            Self::Bsf if entry_id < 0x9a => {
                let height = descriptor_height.ok_or_else(|| {
                    AssetError::InvalidFormat("bsf.cpk requires an embedded row-count descriptor".to_owned())
                })?;
                (1024u16, 800u16, height, height)
            }
            Self::Bsf if entry_id < 0x126 => {
                let height = descriptor_height.ok_or_else(|| {
                    AssetError::InvalidFormat("bsf.cpk requires an embedded row-count descriptor".to_owned())
                })?;
                (1024u16, 768u16, height, height)
            }
            Self::Bsf => {
                return Err(AssetError::InvalidFormat(format!(
                    "bsf.cpk entry ID {entry_id} is outside the engine range 0..293"
                )))
            }
            Self::AddPt | Self::Pt => return Ok(None),
        };
        if descriptor_height == 0 || descriptor_height > capacity_height {
            return Err(AssetError::InvalidFormat(format!(
                "{} embedded descriptor requests {descriptor_height} rows, runtime surface permits 1..={capacity_height}",
                profile_name(self)
            )));
        }
        Ok(Some(RuntimeSurface {
            width,
            capacity_height,
            editable_height,
            descriptor_height,
        }))
    }

    fn decode_chunk_count(self, count_word: u32) -> Result<usize, AssetError> {
        // The shared loader compares a byte-sized active index against the
        // full u32 count. The BSF streaming routine explicitly UXTB-truncates
        // the count before comparison, so its upper 24 bits are opaque and
        // must be preserved rather than interpreted as part of the count.
        // Both routines perform one chunk before the first comparison.
        let count = match self {
            Self::Bsf => usize::from((count_word & 0xff) as u8),
            Self::AddPt | Self::Bk | Self::Pt => usize::try_from(count_word).map_err(|_| {
                AssetError::InvalidFormat("chunk count overflows usize".to_owned())
            })?,
        };
        if count > MAX_CHUNKS {
            return Err(AssetError::InvalidFormat(format!(
                "invalid chunk count {count}; engine index is one byte and would wrap before completion"
            )));
        }
        // Both consumers execute chunk zero before their first count
        // comparison. A stored count of zero therefore has the same effective
        // one-chunk behavior as a stored count of one.
        Ok(count.max(1))
    }

    fn encode_chunk_count(self, original_word: u32, count: usize) -> Result<u32, AssetError> {
        if count == 0 || count > MAX_CHUNKS {
            return Err(AssetError::InvalidProject(
                "chunk plan must contain 1..=255 chunks".to_owned(),
            ));
        }
        let original_count = match self {
            Self::Bsf => original_word & 0xff,
            Self::AddPt | Self::Bk | Self::Pt => original_word,
        };
        // Preserve the engine's special zero encoding when the original table
        // used it for its effective single-chunk do-while execution.
        let encoded = if count == 1 && original_count == 0 {
            0
        } else {
            u32::try_from(count).unwrap()
        };
        Ok(match self {
            Self::Bsf => (original_word & 0xffff_ff00) | encoded,
            Self::AddPt | Self::Bk | Self::Pt => encoded,
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct RuntimeSurface {
    width: u16,
    capacity_height: u16,
    editable_height: u16,
    descriptor_height: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnginePackageDocument {
    pub document_version: u32,
    pub profile: EnginePackageProfile,
    pub entry_id: u32,
    /// Fixed number of bytes loaded by the engine's executable-resident sector table.
    pub allocation_size: u32,
    pub wrapped_bundle: bool,
    /// Exact outer-bundle bytes from offset zero through the first physical
    /// subpackage. The engine-defined count/offset fields are overlaid during
    /// build; all other bytes remain authoritative because no consumer was
    /// found that defines them as disposable padding.
    pub bundle_prefix_hex: String,
    pub bundle_alignment: u32,
    /// Logical package index -> physically stored subpackage index. Duplicate and
    /// non-monotonic outer offsets are representable because the engine performs
    /// independent indexed lookups and does not compare neighboring offsets.
    pub bundle_map: Vec<u32>,
    pub subpackages: Vec<EngineSubPackageDocument>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineSubPackageDocument {
    /// Original package length before sector padding. Build allocates a new
    /// zero-filled package of this size; it never reads a skeleton.bin file.
    pub package_size: u32,
    /// Explicit preservation layer for fields/bytes whose producer semantics
    /// are not yet proven. The recognized compressed table span is zeroed.
    /// This is visible JSON data, not a hidden binary fallback; known fields,
    /// palette and regenerated chunks are validated and overlaid during build.
    pub preserved_layout_hex: String,
    pub original_chunk_region_offset: u32,
    pub chunk_stride: u32,
    pub descriptor_offset: u32,
    pub texture_binding: EngineTextureBinding,
    /// Bytes addressed by the editable PNG representation.
    pub texture_data_size: u32,
    /// Final extent reached by the engine's ordered chunk writes.
    pub decoded_size: u32,
    /// Exact post-GZIP GPU byte buffer observed at extraction. rz-tool 1.0 always
    /// writes this machine-managed file because PNG conversion is not a
    /// bijection for block-compressed or indexed textures.
    pub preserved_decoded: Option<String>,
    /// FNV-1a fingerprint of the exact source decoded buffer. The file is
    /// machine-managed and must not be edited independently of the PNG.
    pub preserved_decoded_fnv1a64: String,
    pub texture: EngineTextureLayout,
    pub palette: Option<EnginePaletteDocument>,
    pub layers: Vec<EnginePackageLayer>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "binding", rename_all = "kebab-case")]
pub enum EngineTextureBinding {
    EmbeddedDescriptor,
    RuntimeRgba8 {
        width: u16,
        capacity_height: u16,
        editable_height: u16,
        descriptor_height: u16,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnginePaletteDocument {
    pub offset: u32,
    pub low_nibble_first: bool,
    pub colors: Vec<[u8; 4]>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnginePackageLayer {
    pub table_offset: u32,
    pub original_table_span: u32,
    pub first_block_offset: u32,
    /// Original table count word. BSF consumes only its low byte; the upper
    /// bytes are preserved because their meaning is not established.
    pub count_word: u32,
    /// Exact bytes from table start through the first compressed block. Build
    /// overlays the count and offsets, preserving unknown header/padding data.
    pub table_prefix_hex: String,
    pub chunks: Vec<EngineChunkDocument>,
    /// Exact source chunk table, including GZIP streams and alignment bytes.
    /// It is reused when the editable PNG maps to the original GPU bytes.
    pub source_table: String,
    pub source_table_fnv1a64: String,
    pub image: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EngineChunkDocument {
    pub output_size: u32,
    pub reserved_words: [u32; 3],
}

#[derive(Debug, Clone, Copy)]
struct PackageHeader {
    descriptor_offset: usize,
    aux_offset: usize,
    chunk_region_offset: usize,
    chunk_stride: usize,
}

#[derive(Debug, Clone)]
struct ParsedTexture {
    layout: EngineTextureLayout,
    data_size: usize,
    palette: Option<EnginePaletteDocument>,
    binding: EngineTextureBinding,
}

struct EncodedPackageTexture {
    bytes: Vec<u8>,
    palette: Option<EnginePaletteDocument>,
    exact_visual_roundtrip: bool,
}

struct ParsedChunkTable {
    first_block_offset: u32,
    count_word: u32,
    table_prefix: Vec<u8>,
    table_size: usize,
    chunks: Vec<EngineChunkDocument>,
    decompressed: Vec<u8>,
    ranges: Vec<(usize, usize)>,
}

struct ParsedBundle {
    prefix: Vec<u8>,
    ranges: Vec<(usize, usize)>,
    logical_to_physical: Vec<u32>,
}

pub fn looks_like_package(input: &[u8]) -> bool {
    parse_bundle(input).is_ok()
        || [EnginePackageProfile::Bk, EnginePackageProfile::Bsf]
            .into_iter()
            .any(|profile| validate_single_header(input, profile).is_ok())
}

pub fn decode(
    input: &[u8],
    asset_directory: &Path,
    output_stem: &str,
    profile: EnginePackageProfile,
    entry_id: u32,
    allocation_size: usize,
) -> Result<AssetKind, AssetError> {
    if input.len() > allocation_size {
        return Err(AssetError::InvalidFormat(format!(
            "{} entry ID {entry_id} contains {:#x} bytes, but the engine sector table allocates only {allocation_size:#x}",
            profile_name(profile),
            input.len()
        )));
    }
    let mut working = input.to_vec();
    working.resize(allocation_size, 0);
    verify_engine_integrity_footer(&working).map_err(|error| {
        AssetError::InvalidProject(format!(
            "source {} entry ID {entry_id} integrity footer mismatch: {error}",
            profile_name(profile)
        ))
    })?;

    let (wrapped_bundle, bundle_prefix, bundle_map, ranges) = if profile.uses_bundle() {
        let bundle = parse_bundle(&working)?;
        if bundle.ranges.is_empty() {
            return Err(AssetError::InvalidFormat(
                "engine image bundle contains no physical package".to_owned(),
            ));
        }
        (
            true,
            bundle.prefix,
            bundle.logical_to_physical,
            bundle.ranges,
        )
    } else {
        validate_single_header(&working, profile)?;
        (false, Vec::new(), vec![0], vec![(0usize, working.len())])
    };

    let bundle_alignment = infer_bundle_alignment(&ranges);
    let mut subpackages = Vec::with_capacity(ranges.len());
    for (index, (start, end)) in ranges.into_iter().enumerate() {
        let subpackage_stem = format!("{output_stem}-{index:03}");
        let document = decode_subpackage(
            &working[start..end],
            asset_directory,
            &subpackage_stem,
            profile,
            entry_id,
        )?;
        subpackages.push(document);
    }

    let document = EnginePackageDocument {
        document_version: DOCUMENT_VERSION,
        profile,
        entry_id,
        allocation_size: u32::try_from(allocation_size).map_err(|_| {
            AssetError::InvalidFormat("engine allocation exceeds u32".to_owned())
        })?,
        wrapped_bundle,
        bundle_prefix_hex: encode_hex(&bundle_prefix),
        bundle_alignment,
        bundle_map,
        subpackages,
    };
    let relative = format!("{output_stem}.package.json");
    fs::write(
        asset_directory.join(&relative),
        serde_json::to_vec_pretty(&document)?,
    )?;
    Ok(AssetKind::EngineImagePackage {
        document: relative,
    })
}

pub fn encode(document_path: &Path) -> Result<Vec<u8>, AssetError> {
    let document: EnginePackageDocument = serde_json::from_slice(&fs::read(document_path)?)?;
    if document.document_version != DOCUMENT_VERSION {
        return Err(AssetError::InvalidProject(format!(
            "engine package document version {} is unsupported",
            document.document_version
        )));
    }
    if document.wrapped_bundle != document.profile.uses_bundle() {
        return Err(AssetError::InvalidProject(
            "package.json bundle mode does not match the engine archive profile".to_owned(),
        ));
    }
    if !document.wrapped_bundle && !document.bundle_prefix_hex.is_empty() {
        return Err(AssetError::InvalidProject(
            "unwrapped package must not contain an outer bundle prefix".to_owned(),
        ));
    }
    if document.subpackages.is_empty() {
        return Err(AssetError::InvalidProject(
            "engine package contains no physical subpackages".to_owned(),
        ));
    }
    let allocation_size = usize::try_from(document.allocation_size).map_err(|_| {
        AssetError::InvalidProject("engine allocation size overflows usize".to_owned())
    })?;
    let root = document_path.parent().unwrap_or_else(|| Path::new("."));
    let mut packages = Vec::with_capacity(document.subpackages.len());
    for subpackage in &document.subpackages {
        packages.push(encode_subpackage(
            subpackage,
            root,
            document.profile,
            document.entry_id,
        )?);
    }

    let mut output = if !document.wrapped_bundle {
        if packages.len() != 1 || document.bundle_map.as_slice() != [0] {
            return Err(AssetError::InvalidProject(
                "unwrapped package document must contain one package mapped as index zero"
                    .to_owned(),
            ));
        }
        packages.remove(0)
    } else {
        build_bundle(&document, &packages)?
    };

    if output.len() > allocation_size {
        return Err(AssetError::InvalidProject(format!(
            "rebuilt {} entry ID {} is {:#x} bytes, exceeding the engine's fixed {allocation_size:#x}-byte sector allocation",
            profile_name(document.profile),
            document.entry_id,
            output.len()
        )));
    }
    output.resize(allocation_size, 0);
    regenerate_engine_integrity_footer(&mut output).map_err(|error| {
        AssetError::InvalidProject(format!(
            "failed to generate {} entry ID {} integrity footer: {error}",
            profile_name(document.profile),
            document.entry_id
        ))
    })?;
    verify_engine_integrity_footer(&output).map_err(|error| {
        AssetError::InvalidProject(format!(
            "rebuilt {} entry ID {} failed integrity verification: {error}",
            profile_name(document.profile),
            document.entry_id
        ))
    })?;
    Ok(output)
}

fn decode_subpackage(
    input: &[u8],
    output_directory: &Path,
    output_stem: &str,
    profile: EnginePackageProfile,
    entry_id: u32,
) -> Result<EngineSubPackageDocument, AssetError> {
    let header = validate_single_header(input, profile)?;
    let descriptor = if profile == EnginePackageProfile::Bk {
        None
    } else {
        Some(
            input
                .get(header.descriptor_offset..header.descriptor_offset + TEXTURE_DESCRIPTOR_SIZE)
                .ok_or_else(|| AssetError::InvalidFormat("truncated texture descriptor".to_owned()))?,
        )
    };
    let parsed_texture = parse_texture(profile, entry_id, input, header, descriptor)?;
    let table_offset = profile.table_offset();
    let table = parse_chunk_table(
        input,
        header.chunk_region_offset,
        table_offset,
        header.chunk_stride,
        profile.maximum_decode_size(entry_id)?,
        profile,
    )
    .map_err(|error| {
        AssetError::InvalidFormat(format!(
            "{} image table at {table_offset:#x} is invalid: {error}",
            profile_name(profile)
        ))
    })?;
    if table.decompressed.len() < parsed_texture.data_size {
        return Err(AssetError::InvalidFormat(format!(
            "{} image table reaches only {} decoded bytes, but the engine copy path addresses {}",
            profile_name(profile),
            table.decompressed.len(),
            parsed_texture.data_size
        )));
    }
    validate_visible_coverage(&table.ranges, parsed_texture.data_size)?;

    let image = decode_package_texture(
        &table.decompressed[..parsed_texture.data_size],
        parsed_texture.layout,
        parsed_texture.palette.as_ref(),
    )?;
    let image_name = format!("{output_stem}.png");
    image.save(output_directory.join(&image_name))?;

    // The engine receives this exact post-GZIP byte buffer. Preserve it even
    // when it contains only visible texture bytes: decoding to PNG and encoding
    // back is not bijective for BC formats and can also change palette indices.
    let preserved_decoded_name = format!("{output_stem}.decoded-source.bin");
    fs::write(
        output_directory.join(&preserved_decoded_name),
        &table.decompressed,
    )?;
    let preserved_decoded_fnv1a64 = fnv1a64_hex(&table.decompressed);

    let absolute_start = header
        .chunk_region_offset
        .checked_add(table_offset)
        .ok_or_else(|| AssetError::InvalidFormat("chunk table offset overflow".to_owned()))?;
    let absolute_end = absolute_start
        .checked_add(table.table_size)
        .ok_or_else(|| AssetError::InvalidFormat("chunk table size overflow".to_owned()))?;
    let source_table = input[absolute_start..absolute_end].to_vec();
    let source_table_name = format!("{output_stem}.chunk-table-source.bin");
    fs::write(output_directory.join(&source_table_name), &source_table)?;
    let source_table_fnv1a64 = fnv1a64_hex(&source_table);

    let mut preserved_layout = input.to_vec();
    preserved_layout[absolute_start..absolute_end].fill(0);

    Ok(EngineSubPackageDocument {
        package_size: u32::try_from(input.len()).map_err(|_| {
            AssetError::InvalidFormat("engine package size exceeds u32".to_owned())
        })?,
        preserved_layout_hex: encode_hex(&preserved_layout),
        original_chunk_region_offset: u32::try_from(header.chunk_region_offset).map_err(|_| {
            AssetError::InvalidFormat("chunk region offset exceeds u32".to_owned())
        })?,
        chunk_stride: u32::try_from(header.chunk_stride).map_err(|_| {
            AssetError::InvalidFormat("chunk stride exceeds u32".to_owned())
        })?,
        descriptor_offset: u32::try_from(header.descriptor_offset).map_err(|_| {
            AssetError::InvalidFormat("descriptor offset exceeds u32".to_owned())
        })?,
        texture_binding: parsed_texture.binding,
        texture_data_size: u32::try_from(parsed_texture.data_size).map_err(|_| {
            AssetError::InvalidFormat("texture data size exceeds u32".to_owned())
        })?,
        decoded_size: u32::try_from(table.decompressed.len()).map_err(|_| {
            AssetError::InvalidFormat("decoded image buffer exceeds u32".to_owned())
        })?,
        preserved_decoded: Some(preserved_decoded_name),
        preserved_decoded_fnv1a64,
        texture: parsed_texture.layout,
        palette: parsed_texture.palette,
        layers: vec![EnginePackageLayer {
            table_offset: u32::try_from(table_offset).unwrap(),
            original_table_span: u32::try_from(table.table_size).map_err(|_| {
                AssetError::InvalidFormat("chunk table span exceeds u32".to_owned())
            })?,
            first_block_offset: table.first_block_offset,
            count_word: table.count_word,
            table_prefix_hex: encode_hex(&table.table_prefix),
            chunks: table.chunks,
            source_table: source_table_name,
            source_table_fnv1a64,
            image: image_name,
        }],
    })
}

fn parse_texture(
    profile: EnginePackageProfile,
    entry_id: u32,
    package: &[u8],
    header: PackageHeader,
    descriptor: Option<&[u8]>,
) -> Result<ParsedTexture, AssetError> {
    let descriptor_height = descriptor
        .map(|bytes| read_u16(bytes, 0x2a))
        .transpose()?;
    if let Some(surface) = profile.runtime_surface(entry_id, descriptor_height)? {
        let data_size = usize::from(surface.width)
            .checked_mul(usize::from(surface.editable_height))
            .and_then(|value| value.checked_mul(4))
            .ok_or_else(|| AssetError::InvalidFormat("runtime texture size overflow".to_owned()))?;
        return Ok(ParsedTexture {
            layout: EngineTextureLayout {
                palette_index: u32::MAX,
                flags: 0,
                texture_type: TEXTURE_TYPE_LINEAR,
                format: FORMAT_RUNTIME_RGBA8,
                width: surface.width,
                height: surface.editable_height,
                mip_count: 0,
            },
            data_size,
            palette: None,
            binding: EngineTextureBinding::RuntimeRgba8 {
                width: surface.width,
                capacity_height: surface.capacity_height,
                editable_height: surface.editable_height,
                descriptor_height: surface.descriptor_height,
            },
        });
    }

    let descriptor = descriptor.ok_or_else(|| {
        AssetError::InvalidFormat("embedded texture descriptor is unavailable".to_owned())
    })?;
    let runtime_gxt_pointer = read_u32(descriptor, 0x10)?;
    if profile.creates_texture_from_descriptor() && runtime_gxt_pointer != 0 {
        return Err(AssetError::InvalidFormat(format!(
            "texture descriptor contains runtime pointer {runtime_gxt_pointer:#010x}; the engine dereferences it as an in-memory object"
        )));
    }
    let data_size = usize::try_from(read_u32(descriptor, 0x04)?).map_err(|_| {
        AssetError::InvalidFormat("direct texture size overflows usize".to_owned())
    })?;
    let palette_size = usize::try_from(read_u32(descriptor, 0x14)?).map_err(|_| {
        AssetError::InvalidFormat("palette size overflows usize".to_owned())
    })?;
    let layout = EngineTextureLayout {
        palette_index: u32::MAX,
        flags: 0,
        texture_type: read_u32(descriptor, 0x1c)?,
        format: read_u32(descriptor, 0x20)?,
        width: read_u16(descriptor, 0x28)?,
        height: read_u16(descriptor, 0x2a)?,
        mip_count: 0,
    };
    if data_size == 0 || layout.width == 0 || layout.height == 0 {
        return Err(AssetError::InvalidFormat(
            "direct texture descriptor has zero size or dimensions".to_owned(),
        ));
    }

    let base = layout.format & FORMAT_BASE_MASK;
    let palette = match base {
        FORMAT_P4 | FORMAT_P8 => {
            if layout.texture_type != TEXTURE_TYPE_LINEAR {
                return Err(AssetError::InvalidFormat(format!(
                    "paletted engine texture type {:#010x} is not proven by the analyzed copy path; only linear P4/P8 is editable",
                    layout.texture_type
                )));
            }
            let expected_palette_size = if base == FORMAT_P4 { 0x40 } else { 0x400 };
            if palette_size != expected_palette_size {
                return Err(AssetError::InvalidFormat(format!(
                    "paletted texture declares palette size {palette_size:#x}, expected {expected_palette_size:#x}"
                )));
            }
            let pixel_count = usize::from(layout.width)
                .checked_mul(usize::from(layout.height))
                .ok_or_else(|| AssetError::InvalidFormat("texture pixel count overflow".to_owned()))?;
            let expected_data_size = if base == FORMAT_P4 {
                pixel_count.div_ceil(2)
            } else {
                pixel_count
            };
            if data_size != expected_data_size {
                return Err(AssetError::InvalidFormat(format!(
                    "paletted texture data size is {data_size:#x}, expected {expected_data_size:#x} for {}x{}",
                    layout.width, layout.height
                )));
            }
            if header.aux_offset == 0 {
                return Err(AssetError::InvalidFormat(
                    "paletted texture has no package palette offset at +0x24".to_owned(),
                ));
            }
            let palette_end = header
                .aux_offset
                .checked_add(palette_size)
                .ok_or_else(|| AssetError::InvalidFormat("palette range overflow".to_owned()))?;
            if palette_end > package.len() {
                return Err(AssetError::InvalidFormat(
                    "palette block exceeds package size".to_owned(),
                ));
            }
            let mut colors = Vec::with_capacity(palette_size / 4);
            for raw in package[header.aux_offset..palette_end].chunks_exact(4) {
                colors.push(decode_uncompressed_pixel(raw, layout.format));
            }
            Some(EnginePaletteDocument {
                offset: u32::try_from(header.aux_offset).unwrap(),
                low_nibble_first: true,
                colors,
            })
        }
        _ => {
            if palette_size != 0 {
                return Err(AssetError::InvalidFormat(format!(
                    "non-paletted format {base:#010x} declares an unexplained palette block of {palette_size:#x} bytes"
                )));
            }
            None
        }
    };

    Ok(ParsedTexture {
        layout,
        data_size,
        palette,
        binding: EngineTextureBinding::EmbeddedDescriptor,
    })
}

fn encode_subpackage(
    document: &EngineSubPackageDocument,
    root: &Path,
    profile: EnginePackageProfile,
    entry_id: u32,
) -> Result<Vec<u8>, AssetError> {
    let package_size = usize::try_from(document.package_size).map_err(|_| {
        AssetError::InvalidProject("engine package size overflows usize".to_owned())
    })?;
    let mut output = decode_hex(&document.preserved_layout_hex).map_err(|error| {
        AssetError::InvalidProject(format!("invalid preserved_layout_hex: {error}"))
    })?;
    if output.len() != package_size {
        return Err(AssetError::InvalidProject(format!(
            "preserved_layout_hex decodes to {} bytes, package_size is {package_size}",
            output.len()
        )));
    }
    if output.len() < PACKAGE_HEADER_SIZE {
        return Err(AssetError::InvalidProject(
            "engine package layout is smaller than the package header".to_owned(),
        ));
    }
    let original_chunk_region_offset = usize::try_from(document.original_chunk_region_offset)
        .map_err(|_| AssetError::InvalidProject("chunk region offset overflows usize".to_owned()))?;
    validate_document_header(document, &output, profile, entry_id)?;

    match &document.texture_binding {
        EngineTextureBinding::EmbeddedDescriptor => {
            if !profile.creates_texture_from_descriptor() {
                return Err(AssetError::InvalidProject(format!(
                    "{} uses an executable-created runtime surface, not an editable embedded texture binding",
                    profile_name(profile)
                )));
            }
            let descriptor_offset = usize::try_from(document.descriptor_offset).map_err(|_| {
                AssetError::InvalidProject("descriptor offset overflows usize".to_owned())
            })?;
            let descriptor_end = descriptor_offset
                .checked_add(TEXTURE_DESCRIPTOR_SIZE)
                .ok_or_else(|| AssetError::InvalidProject("descriptor range overflow".to_owned()))?;
            if descriptor_end > output.len() {
                return Err(AssetError::InvalidProject(
                    "texture descriptor is outside generated package layout".to_owned(),
                ));
            }
            validate_document_descriptor(document, &output[descriptor_offset..descriptor_end])?;
        }
        EngineTextureBinding::RuntimeRgba8 {
            width,
            capacity_height,
            editable_height,
            descriptor_height,
        } => {
            let observed_descriptor_height = if profile == EnginePackageProfile::Bk {
                None
            } else {
                let descriptor_offset = usize::try_from(document.descriptor_offset).map_err(|_| {
                    AssetError::InvalidProject("descriptor offset overflows usize".to_owned())
                })?;
                let descriptor = output
                    .get(descriptor_offset..descriptor_offset + TEXTURE_DESCRIPTOR_SIZE)
                    .ok_or_else(|| AssetError::InvalidProject("runtime descriptor is truncated".to_owned()))?;
                Some(read_u16(descriptor, 0x2a).map_err(|error| {
                    AssetError::InvalidProject(error.to_string())
                })?)
            };
            let expected = profile
                .runtime_surface(entry_id, observed_descriptor_height)
                .map_err(|error| AssetError::InvalidProject(error.to_string()))?
                .ok_or_else(|| {
                    AssetError::InvalidProject(
                        "runtime surface binding is invalid for this profile".to_owned(),
                    )
                })?;
            if *width != expected.width
                || *capacity_height != expected.capacity_height
                || *editable_height != expected.editable_height
                || *descriptor_height != expected.descriptor_height
                || document.texture.width != expected.width
                || document.texture.height != expected.editable_height
                || document.texture.texture_type != TEXTURE_TYPE_LINEAR
                || document.texture.format != FORMAT_RUNTIME_RGBA8
                || document.palette.is_some()
            {
                return Err(AssetError::InvalidProject(
                    "runtime RGBA8 surface metadata differs from the engine profile".to_owned(),
                ));
            }
        }
    }

    let texture_size = usize::try_from(document.texture_data_size).map_err(|_| {
        AssetError::InvalidProject("texture data size overflows usize".to_owned())
    })?;
    let decoded_size = usize::try_from(document.decoded_size).map_err(|_| {
        AssetError::InvalidProject("decoded buffer size overflows usize".to_owned())
    })?;
    if texture_size > decoded_size {
        return Err(AssetError::InvalidProject(
            "editable texture exceeds preserved decoded buffer".to_owned(),
        ));
    }
    let preserved_decoded_path = document.preserved_decoded.as_ref().ok_or_else(|| {
        AssetError::InvalidProject(
            "rz-tool 1.0 image projects require preserved_decoded; re-extract with rz-tool 1.0"
                .to_owned(),
        )
    })?;
    let mut decoded = fs::read(root.join(preserved_decoded_path))?;
    if decoded.len() != decoded_size {
        return Err(AssetError::InvalidProject(format!(
            "preserved decoded buffer has {} bytes, expected {decoded_size}",
            decoded.len()
        )));
    }
    if fnv1a64_hex(&decoded) != document.preserved_decoded_fnv1a64 {
        return Err(AssetError::InvalidProject(
            "preserved decoded buffer fingerprint mismatch; re-extract instead of rebuilding potentially corrupt texture data"
                .to_owned(),
        ));
    }

    if document.layers.len() != 1
        || document.layers[0].table_offset != u32::try_from(profile.table_offset()).unwrap()
    {
        return Err(AssetError::InvalidProject(format!(
            "{} must contain exactly one layer at table offset {:#x}",
            profile_name(profile),
            profile.table_offset()
        )));
    }
    let layer = &document.layers[0];
    let stride = usize::try_from(document.chunk_stride).map_err(|_| {
        AssetError::InvalidProject("chunk stride overflows usize".to_owned())
    })?;
    validate_chunk_plan(&layer.chunks, stride, decoded_size)?;
    let first_block_offset = usize::try_from(layer.first_block_offset).map_err(|_| {
        AssetError::InvalidProject("first block offset overflows usize".to_owned())
    })?;
    let table_prefix = decode_hex(&layer.table_prefix_hex).map_err(|error| {
        AssetError::InvalidProject(format!("invalid table_prefix_hex: {error}"))
    })?;
    if table_prefix.len() != first_block_offset {
        return Err(AssetError::InvalidProject(format!(
            "table_prefix_hex decodes to {} bytes, expected {first_block_offset}",
            table_prefix.len()
        )));
    }
    let original_span = usize::try_from(layer.original_table_span).map_err(|_| {
        AssetError::InvalidProject("original chunk table span overflows usize".to_owned())
    })?;
    let source_table = fs::read(root.join(&layer.source_table))?;
    if source_table.len() != original_span {
        return Err(AssetError::InvalidProject(format!(
            "source chunk table has {} bytes, expected {original_span}",
            source_table.len()
        )));
    }
    if fnv1a64_hex(&source_table) != layer.source_table_fnv1a64 {
        return Err(AssetError::InvalidProject(
            "source chunk-table fingerprint mismatch; re-extract instead of rebuilding potentially corrupt texture data"
                .to_owned(),
        ));
    }
    let source_parsed = parse_chunk_table(
        &source_table,
        0,
        0,
        stride,
        profile
            .maximum_decode_size(entry_id)
            .map_err(|error| AssetError::InvalidProject(error.to_string()))?,
        profile,
    )
    .map_err(|error| {
        AssetError::InvalidProject(format!("preserved source chunk table is invalid: {error}"))
    })?;
    if source_parsed.table_size != source_table.len()
        || source_parsed.first_block_offset != layer.first_block_offset
        || source_parsed.count_word != layer.count_word
        || source_parsed.table_prefix.as_slice() != table_prefix.as_slice()
        || source_parsed.chunks.as_slice() != layer.chunks.as_slice()
        || source_parsed.decompressed.as_slice() != decoded.as_slice()
    {
        return Err(AssetError::InvalidProject(
            "package JSON, source chunk table, and preserved decoded buffer disagree; re-extract before rebuilding"
                .to_owned(),
        ));
    }

    let source_decoded = decoded.clone();
    let image = image::open(root.join(&layer.image))?.to_rgba8();
    let source_image = decode_package_texture(
        &source_decoded[..texture_size],
        document.texture,
        document.palette.as_ref(),
    )
    .map_err(|error| AssetError::InvalidProject(error.to_string()))?;
    let texture_changed = image != source_image;
    let mut effective_palette = document.palette.clone();
    if texture_changed {
        let encoded_texture = encode_package_texture(
            &image,
            document.texture,
            document.palette.as_ref(),
            texture_size,
            &source_decoded[..texture_size],
        )?;
        decoded[..texture_size].copy_from_slice(&encoded_texture.bytes);
        effective_palette = encoded_texture.palette;
        let roundtrip = decode_package_texture(
            &decoded[..texture_size],
            document.texture,
            effective_palette.as_ref(),
        )
        .map_err(|error| AssetError::InvalidProject(format!(
            "re-encoded texture cannot be decoded by the engine model: {error}"
        )))?;
        if encoded_texture.exact_visual_roundtrip {
            if roundtrip != image {
                return Err(AssetError::InvalidProject(
                    "reversible texture encoder did not reproduce the edited PNG exactly".to_owned(),
                ));
            }
        } else {
            let source_error = texture_visual_error(&source_image, &image, document.texture.format)?;
            let rebuilt_error = texture_visual_error(&roundtrip, &image, document.texture.format)?;
            if roundtrip == source_image {
                return Err(AssetError::InvalidProject(format!(
                    "edited texture has no representable visual effect in the target GPU format (source error {source_error}, encoded error {rebuilt_error})"
                )));
            }
        }
    }
    write_palette_to_package(
        document.texture,
        effective_palette.as_ref(),
        &mut output,
    )?;

    // Preserve each unchanged compressed block exactly and recompress only the
    // chunks whose post-GZIP destination bytes changed. This follows the engine
    // loader's index*stride write model without turning editability into a
    // copy-only mode.
    let table = build_chunk_table_incremental(
        profile,
        &decoded,
        &source_decoded,
        &source_table,
        &layer.chunks,
        stride,
        first_block_offset,
        layer.count_word,
        &table_prefix,
    )?;
    let table_offset = usize::try_from(layer.table_offset).map_err(|_| {
        AssetError::InvalidProject("chunk table offset overflows usize".to_owned())
    })?;

    let can_reuse = table.len() <= original_span
        && original_chunk_region_offset
            .checked_add(table_offset)
            .and_then(|start| start.checked_add(original_span))
            .is_some_and(|end| end <= output.len());

    let chunk_region_offset = if can_reuse {
        let start = original_chunk_region_offset + table_offset;
        output[start..start + original_span].fill(0);
        output[start..start + table.len()].copy_from_slice(&table);
        original_chunk_region_offset
    } else {
        let relocated = align_up(output.len(), 4)?;
        let prefix = output
            .get(original_chunk_region_offset..original_chunk_region_offset + table_offset)
            .ok_or_else(|| {
                AssetError::InvalidProject(
                    "original chunk-region prefix is outside generated package layout".to_owned(),
                )
            })?
            .to_vec();
        let required = relocated
            .checked_add(table_offset)
            .and_then(|value| value.checked_add(table.len()))
            .ok_or_else(|| AssetError::InvalidProject("relocated chunk region overflow".to_owned()))?;
        output.resize(required, 0);
        output[relocated..relocated + prefix.len()].copy_from_slice(&prefix);
        let start = relocated + table_offset;
        output[start..start + table.len()].copy_from_slice(&table);
        relocated
    };

    write_u32(
        &mut output,
        0x34,
        u32::try_from(chunk_region_offset).map_err(|_| {
            AssetError::InvalidProject("chunk region offset exceeds u32".to_owned())
        })?,
    )?;
    verify_rebuilt_subpackage(
        &output,
        document,
        profile,
        entry_id,
        effective_palette.as_ref(),
        &decoded,
    )?;
    Ok(output)
}

fn verify_rebuilt_subpackage(
    output: &[u8],
    document: &EngineSubPackageDocument,
    profile: EnginePackageProfile,
    entry_id: u32,
    expected_palette: Option<&EnginePaletteDocument>,
    expected_decoded: &[u8],
) -> Result<(), AssetError> {
    let header = validate_single_header(output, profile)
        .map_err(|error| AssetError::InvalidProject(format!(
            "rebuilt package header failed engine-model validation: {error}"
        )))?;
    let descriptor = if profile == EnginePackageProfile::Bk {
        None
    } else {
        Some(
            output
                .get(header.descriptor_offset..header.descriptor_offset + TEXTURE_DESCRIPTOR_SIZE)
                .ok_or_else(|| {
                    AssetError::InvalidProject(
                        "rebuilt texture descriptor is truncated".to_owned(),
                    )
                })?,
        )
    };
    let texture = parse_texture(profile, entry_id, output, header, descriptor)
        .map_err(|error| AssetError::InvalidProject(format!(
            "rebuilt texture descriptor failed engine-model validation: {error}"
        )))?;
    if texture.layout != document.texture
        || texture.data_size
            != usize::try_from(document.texture_data_size).map_err(|_| {
                AssetError::InvalidProject("texture data size overflows usize".to_owned())
            })?
        || texture.palette.as_ref() != expected_palette
        || &texture.binding != &document.texture_binding
    {
        return Err(AssetError::InvalidProject(
            "rebuilt texture descriptor/palette differs from the effective encode state".to_owned(),
        ));
    }

    let layer = document.layers.first().ok_or_else(|| {
        AssetError::InvalidProject("rebuilt package has no layer metadata".to_owned())
    })?;
    let table = parse_chunk_table(
        output,
        header.chunk_region_offset,
        profile.table_offset(),
        header.chunk_stride,
        profile
            .maximum_decode_size(entry_id)
            .map_err(|error| AssetError::InvalidProject(error.to_string()))?,
        profile,
    )
    .map_err(|error| AssetError::InvalidProject(format!(
        "rebuilt chunk table failed engine-model validation: {error}"
    )))?;
    if table.decompressed.as_slice() != expected_decoded {
        return Err(AssetError::InvalidProject(
            "rebuilt GZIP table does not reproduce the intended GPU byte buffer"
                .to_owned(),
        ));
    }
    if table.count_word != layer.count_word
        || table.chunks.as_slice() != layer.chunks.as_slice()
    {
        return Err(AssetError::InvalidProject(
            "rebuilt chunk count/reserved metadata differs from package.json".to_owned(),
        ));
    }
    validate_visible_coverage(
        &table.ranges,
        usize::try_from(document.texture_data_size).map_err(|_| {
            AssetError::InvalidProject("texture data size overflows usize".to_owned())
        })?,
    )
    .map_err(|error| AssetError::InvalidProject(error.to_string()))?;
    Ok(())
}

fn build_bundle(
    document: &EnginePackageDocument,
    packages: &[Vec<u8>],
) -> Result<Vec<u8>, AssetError> {
    if document.bundle_map.is_empty() || document.bundle_map.len() > u8::MAX as usize {
        return Err(AssetError::InvalidProject(
            "bundle_map must contain 1..=255 logical packages".to_owned(),
        ));
    }
    for &physical in &document.bundle_map {
        if usize::try_from(physical).map_or(true, |index| index >= packages.len()) {
            return Err(AssetError::InvalidProject(format!(
                "bundle_map references missing physical package {physical}"
            )));
        }
    }
    let table_size = 4usize
        .checked_add(document.bundle_map.len().checked_mul(4).ok_or_else(|| {
            AssetError::InvalidProject("bundle offset table overflow".to_owned())
        })?)
        .ok_or_else(|| AssetError::InvalidProject("bundle header overflow".to_owned()))?;
    let alignment = usize::try_from(document.bundle_alignment.max(4)).map_err(|_| {
        AssetError::InvalidProject("bundle alignment overflows usize".to_owned())
    })?;
    if !alignment.is_power_of_two() {
        return Err(AssetError::InvalidProject(
            "bundle alignment must be a power of two".to_owned(),
        ));
    }
    let mut output = decode_hex(&document.bundle_prefix_hex).map_err(|error| {
        AssetError::InvalidProject(format!("invalid bundle_prefix_hex: {error}"))
    })?;
    if output.len() < table_size {
        return Err(AssetError::InvalidProject(format!(
            "preserved bundle prefix has {} bytes, smaller than count/offset table {table_size}",
            output.len()
        )));
    }
    if output.len() % 4 != 0 {
        return Err(AssetError::InvalidProject(
            "preserved bundle prefix is not four-byte aligned".to_owned(),
        ));
    }
    output[0] = u8::try_from(document.bundle_map.len()).unwrap();

    let mut physical_offsets = Vec::with_capacity(packages.len());
    for (index, package) in packages.iter().enumerate() {
        let offset = if index == 0 {
            output.len()
        } else {
            let offset = align_up(output.len(), alignment)?;
            output.resize(offset, 0);
            offset
        };
        physical_offsets.push(offset);
        output.extend_from_slice(package);
    }
    for (logical, physical) in document.bundle_map.iter().copied().enumerate() {
        let physical_index = usize::try_from(physical).unwrap();
        let offset = u32::try_from(physical_offsets[physical_index]).map_err(|_| {
            AssetError::InvalidProject("bundle package offset exceeds u32".to_owned())
        })?;
        output[4 + logical * 4..8 + logical * 4].copy_from_slice(&offset.to_le_bytes());
    }
    Ok(output)
}

fn validate_single_header(
    input: &[u8],
    profile: EnginePackageProfile,
) -> Result<PackageHeader, AssetError> {
    if input.len() < PACKAGE_HEADER_SIZE {
        return Err(AssetError::InvalidFormat(
            "truncated engine image package header".to_owned(),
        ));
    }
    let descriptor_offset = usize::try_from(read_u32(input, 0x04)?).map_err(|_| {
        AssetError::InvalidFormat("descriptor offset overflows usize".to_owned())
    })?;
    let metadata_count = usize::try_from(read_u32(input, 0x10)?).map_err(|_| {
        AssetError::InvalidFormat("metadata count overflows usize".to_owned())
    })?;
    let metadata_offset = usize::try_from(read_u32(input, 0x14)?).map_err(|_| {
        AssetError::InvalidFormat("metadata offset overflows usize".to_owned())
    })?;
    let aux_offset = usize::try_from(read_u32(input, 0x24)?).map_err(|_| {
        AssetError::InvalidFormat("palette/aux offset overflows usize".to_owned())
    })?;
    let chunk_region_offset = usize::try_from(read_u32(input, 0x34)?).map_err(|_| {
        AssetError::InvalidFormat("chunk region offset overflows usize".to_owned())
    })?;
    let chunk_stride = usize::try_from(read_u32(input, 0x3c)?).map_err(|_| {
        AssetError::InvalidFormat("chunk stride overflows usize".to_owned())
    })?;

    if profile != EnginePackageProfile::Bk {
        let descriptor_end = descriptor_offset
            .checked_add(TEXTURE_DESCRIPTOR_SIZE)
            .ok_or_else(|| AssetError::InvalidFormat("descriptor range overflow".to_owned()))?;
        if descriptor_end > input.len() {
            return Err(AssetError::InvalidFormat(format!(
                "texture descriptor range {descriptor_offset:#x}..{descriptor_end:#x} exceeds package size {:#x}",
                input.len()
            )));
        }
    }
    let table_start = chunk_region_offset
        .checked_add(profile.table_offset())
        .ok_or_else(|| AssetError::InvalidFormat("chunk table pointer overflow".to_owned()))?;
    if table_start > input.len().saturating_sub(4) {
        return Err(AssetError::InvalidFormat(format!(
            "chunk table pointer {chunk_region_offset:#x}+{:#x} is outside package size {:#x}",
            profile.table_offset(),
            input.len()
        )));
    }

    if profile.uses_metadata_records() && metadata_count != 0 {
        // FUN_810225fa adds this package's record count to a per-bank cursor
        // and rejects totals >= 0x400 before copying count * 0x20 bytes.
        // A single package count >= 0x400 can therefore never be accepted,
        // independently of the unknown current cursor value.
        if metadata_count >= 0x400 {
            return Err(AssetError::InvalidFormat(format!(
                "metadata record count {metadata_count:#x} cannot fit the engine's <0x400-record pool"
            )));
        }
        let metadata_size = metadata_count.checked_mul(0x20).ok_or_else(|| {
            AssetError::InvalidFormat("metadata table size overflow".to_owned())
        })?;
        let metadata_end = metadata_offset.checked_add(metadata_size).ok_or_else(|| {
            AssetError::InvalidFormat("metadata table range overflow".to_owned())
        })?;
        if metadata_end > input.len() {
            return Err(AssetError::InvalidFormat(format!(
                "metadata table range {metadata_offset:#x}+{metadata_size:#x} exceeds package size {:#x}",
                input.len()
            )));
        }
    }

    Ok(PackageHeader {
        descriptor_offset,
        aux_offset,
        chunk_region_offset,
        chunk_stride,
    })
}

fn validate_document_header(
    document: &EngineSubPackageDocument,
    prefix: &[u8],
    profile: EnginePackageProfile,
    entry_id: u32,
) -> Result<(), AssetError> {
    let chunk_region_offset = read_u32(prefix, 0x34)
        .map_err(|error| AssetError::InvalidProject(error.to_string()))?;
    let chunk_stride = read_u32(prefix, 0x3c)
        .map_err(|error| AssetError::InvalidProject(error.to_string()))?;
    let descriptor_offset = read_u32(prefix, 0x04)
        .map_err(|error| AssetError::InvalidProject(error.to_string()))?;
    if chunk_region_offset != document.original_chunk_region_offset
        || chunk_stride != document.chunk_stride
        || descriptor_offset != document.descriptor_offset
    {
        return Err(AssetError::InvalidProject(
            "package.json offsets/stride differ from the preserved package-layout header".to_owned(),
        ));
    }
    if profile
        .maximum_decode_size(entry_id)
        .map_err(|error| AssetError::InvalidProject(error.to_string()))?
        < usize::try_from(document.decoded_size).map_err(|_| {
            AssetError::InvalidProject("decoded buffer size overflows usize".to_owned())
        })?
    {
        return Err(AssetError::InvalidProject(
            "decoded buffer exceeds the engine caller's fixed destination".to_owned(),
        ));
    }
    Ok(())
}

fn validate_document_descriptor(
    document: &EngineSubPackageDocument,
    descriptor: &[u8],
) -> Result<(), AssetError> {
    if read_u32(descriptor, 0x10)
        .map_err(|error| AssetError::InvalidProject(error.to_string()))?
        != 0
    {
        return Err(AssetError::InvalidProject(
            "persisted descriptor runtime pointer must remain zero".to_owned(),
        ));
    }
    let expected = [
        (0x04, document.texture_data_size),
        (0x1c, document.texture.texture_type),
        (0x20, document.texture.format),
    ];
    for (offset, value) in expected {
        if read_u32(descriptor, offset)
            .map_err(|error| AssetError::InvalidProject(error.to_string()))?
            != value
        {
            return Err(AssetError::InvalidProject(format!(
                "package.json texture field differs from descriptor +{offset:#x}"
            )));
        }
    }
    if read_u16(descriptor, 0x28)
        .map_err(|error| AssetError::InvalidProject(error.to_string()))?
        != document.texture.width
        || read_u16(descriptor, 0x2a)
            .map_err(|error| AssetError::InvalidProject(error.to_string()))?
            != document.texture.height
    {
        return Err(AssetError::InvalidProject(
            "package.json dimensions differ from the preserved descriptor".to_owned(),
        ));
    }
    Ok(())
}

fn write_palette_to_package(
    texture: EngineTextureLayout,
    palette: Option<&EnginePaletteDocument>,
    output: &mut [u8],
) -> Result<(), AssetError> {
    let base = texture.format & FORMAT_BASE_MASK;
    match base {
        FORMAT_P4 | FORMAT_P8 => {
            let palette = palette.ok_or_else(|| {
                AssetError::InvalidProject("paletted texture has no palette".to_owned())
            })?;
            validate_palette(texture.format, palette)
                .map_err(|error| AssetError::InvalidProject(error.to_string()))?;
            let offset = usize::try_from(palette.offset).map_err(|_| {
                AssetError::InvalidProject("palette offset overflows usize".to_owned())
            })?;
            let size = palette.colors.len().checked_mul(4).ok_or_else(|| {
                AssetError::InvalidProject("palette size overflow".to_owned())
            })?;
            let target = output.get_mut(offset..offset + size).ok_or_else(|| {
                AssetError::InvalidProject("palette range exceeds generated package layout".to_owned())
            })?;
            for (raw, color) in target
                .chunks_exact_mut(4)
                .zip(palette.colors.iter().copied())
            {
                raw.copy_from_slice(&encode_uncompressed_pixel(color, texture.format));
            }
            Ok(())
        }
        _ => {
            if palette.is_some() {
                return Err(AssetError::InvalidProject(
                    "non-paletted texture unexpectedly has palette metadata".to_owned(),
                ));
            }
            Ok(())
        }
    }
}

fn decode_package_texture(
    input: &[u8],
    layout: EngineTextureLayout,
    palette: Option<&EnginePaletteDocument>,
) -> Result<RgbaImage, AssetError> {
    let base = layout.format & FORMAT_BASE_MASK;
    match base {
        FORMAT_P4 | FORMAT_P8 => decode_paletted_texture(
            input,
            layout,
            palette.ok_or_else(|| {
                AssetError::InvalidFormat("paletted texture has no palette".to_owned())
            })?,
        ),
        _ => decode_engine_texture(input, layout),
    }
}

fn encode_package_texture(
    image: &RgbaImage,
    layout: EngineTextureLayout,
    palette: Option<&EnginePaletteDocument>,
    expected_size: usize,
    source_texture: &[u8],
) -> Result<EncodedPackageTexture, AssetError> {
    let base = layout.format & FORMAT_BASE_MASK;
    match base {
        FORMAT_P4 | FORMAT_P8 => encode_paletted_texture(
            image,
            layout,
            palette.ok_or_else(|| {
                AssetError::InvalidProject("paletted texture has no palette".to_owned())
            })?,
            expected_size,
            source_texture,
        ),
        _ => Ok(EncodedPackageTexture {
            bytes: encode_engine_texture_preserving_source(
                image,
                layout,
                expected_size,
                source_texture,
            )?,
            palette: None,
            exact_visual_roundtrip: base == 0x0c00_0000,
        }),
    }
}

fn decode_paletted_texture(
    input: &[u8],
    layout: EngineTextureLayout,
    palette: &EnginePaletteDocument,
) -> Result<RgbaImage, AssetError> {
    if layout.texture_type != TEXTURE_TYPE_LINEAR {
        return Err(AssetError::InvalidFormat(
            "only linear engine P4/P8 textures are supported".to_owned(),
        ));
    }
    validate_palette(layout.format, palette)?;
    let width = usize::from(layout.width);
    let height = usize::from(layout.height);
    let pixel_count = width
        .checked_mul(height)
        .ok_or_else(|| AssetError::InvalidFormat("texture pixel count overflow".to_owned()))?;
    let expected = if layout.format & FORMAT_BASE_MASK == FORMAT_P4 {
        pixel_count.div_ceil(2)
    } else {
        pixel_count
    };
    if input.len() != expected {
        return Err(AssetError::InvalidFormat(format!(
            "paletted texture has {} bytes, expected {expected}",
            input.len()
        )));
    }
    let mut image = RgbaImage::new(u32::from(layout.width), u32::from(layout.height));
    for pixel in 0..pixel_count {
        let index = palette_index_at(input, pixel, layout.format, palette.low_nibble_first);
        let color = palette.colors.get(index).ok_or_else(|| {
            AssetError::InvalidFormat(format!("palette index {index} exceeds color table"))
        })?;
        image.put_pixel(
            u32::try_from(pixel % width).unwrap(),
            u32::try_from(pixel / width).unwrap(),
            Rgba(*color),
        );
    }
    Ok(image)
}

fn encode_paletted_texture(
    image: &RgbaImage,
    layout: EngineTextureLayout,
    source_palette: &EnginePaletteDocument,
    expected_size: usize,
    source_texture: &[u8],
) -> Result<EncodedPackageTexture, AssetError> {
    if image.width() != u32::from(layout.width) || image.height() != u32::from(layout.height) {
        return Err(AssetError::InvalidProject(format!(
            "engine texture is {}x{}, expected {}x{}",
            image.width(), image.height(), layout.width, layout.height
        )));
    }
    if layout.texture_type != TEXTURE_TYPE_LINEAR {
        return Err(AssetError::InvalidProject(
            "only linear engine P4/P8 textures are supported".to_owned(),
        ));
    }
    validate_palette(layout.format, source_palette)
        .map_err(|error| AssetError::InvalidProject(error.to_string()))?;
    let pixel_count = usize::from(layout.width)
        .checked_mul(usize::from(layout.height))
        .ok_or_else(|| AssetError::InvalidProject("texture pixel count overflow".to_owned()))?;
    let base = layout.format & FORMAT_BASE_MASK;
    let required = if base == FORMAT_P4 {
        pixel_count.div_ceil(2)
    } else {
        pixel_count
    };
    if required != expected_size {
        return Err(AssetError::InvalidProject(format!(
            "paletted texture requires {required} bytes, descriptor declares {expected_size}"
        )));
    }
    if source_texture.len() != required {
        return Err(AssetError::InvalidProject(format!(
            "source paletted texture has {} bytes, expected {required}",
            source_texture.len()
        )));
    }

    let capacity = if base == FORMAT_P4 { 16 } else { 256 };
    let (colors, exact_visual_roundtrip) = build_effective_palette(
        image,
        layout,
        source_palette,
        source_texture,
        capacity,
    );
    let effective_palette = EnginePaletteDocument {
        offset: source_palette.offset,
        low_nibble_first: source_palette.low_nibble_first,
        colors,
    };
    validate_palette(layout.format, &effective_palette)
        .map_err(|error| AssetError::InvalidProject(error.to_string()))?;

    let mut output = source_texture.to_vec();
    for (pixel, rgba) in image.pixels().enumerate() {
        let source_index = palette_index_at(
            source_texture,
            pixel,
            layout.format,
            source_palette.low_nibble_first,
        );
        let index = select_palette_index(rgba.0, &effective_palette.colors, source_index);
        if base == FORMAT_P4 {
            let value = u8::try_from(index).unwrap();
            let target = &mut output[pixel / 2];
            let low = pixel & 1 == 0;
            if low == effective_palette.low_nibble_first {
                *target = (*target & 0xf0) | value;
            } else {
                *target = (*target & 0x0f) | (value << 4);
            }
        } else {
            output[pixel] = u8::try_from(index).unwrap();
        }
    }

    Ok(EncodedPackageTexture {
        bytes: output,
        palette: Some(effective_palette),
        exact_visual_roundtrip,
    })
}

fn build_effective_palette(
    image: &RgbaImage,
    layout: EngineTextureLayout,
    source_palette: &EnginePaletteDocument,
    source_texture: &[u8],
    capacity: usize,
) -> (Vec<[u8; 4]>, bool) {
    let mut histogram = BTreeMap::<[u8; 4], usize>::new();
    for pixel in image.pixels() {
        *histogram.entry(pixel.0).or_default() += 1;
    }
    if histogram.len() > capacity {
        let quantized = median_cut_palette(&histogram, capacity);
        return (
            arrange_palette_near_source(&quantized, &source_palette.colors),
            false,
        );
    }

    let wanted = histogram.keys().copied().collect::<Vec<_>>();
    let mut colors = source_palette.colors.clone();
    let mut source_usage = vec![0usize; capacity];
    let mut stable_usage = vec![0usize; capacity];
    for (pixel, rgba) in image.pixels().enumerate() {
        let index = palette_index_at(
            source_texture,
            pixel,
            layout.format,
            source_palette.low_nibble_first,
        );
        source_usage[index] += 1;
        if source_palette.colors[index] == rgba.0 {
            stable_usage[index] += 1;
        }
    }

    // Protect every source index that still represents unchanged pixels. Also
    // protect at least one occurrence of every wanted color already present in
    // the source palette. Duplicate indices are retained whenever enough free
    // slots remain for genuinely new colors.
    let mut protected = stable_usage.iter().map(|usage| *usage > 0).collect::<Vec<_>>();
    for color in &wanted {
        if protected
            .iter()
            .enumerate()
            .any(|(index, keep)| *keep && colors[index] == *color)
        {
            continue;
        }
        if let Some(index) = colors
            .iter()
            .enumerate()
            .filter(|(_, candidate)| **candidate == *color)
            .max_by_key(|(index, _)| (stable_usage[*index], source_usage[*index], usize::MAX - *index))
            .map(|(index, _)| index)
        {
            protected[index] = true;
        }
    }

    let new_colors = wanted
        .iter()
        .copied()
        .filter(|color| !colors.iter().any(|candidate| candidate == color))
        .collect::<Vec<_>>();
    let free_count = protected.iter().filter(|keep| !**keep).count();
    if free_count < new_colors.len() {
        let mut protected_per_color = BTreeMap::<[u8; 4], usize>::new();
        for (index, keep) in protected.iter().copied().enumerate() {
            if keep {
                *protected_per_color.entry(colors[index]).or_default() += 1;
            }
        }
        let mut releasable = (0..capacity)
            .filter(|index| {
                protected[*index]
                    && protected_per_color.get(&colors[*index]).copied().unwrap_or(0) > 1
            })
            .collect::<Vec<_>>();
        releasable.sort_by_key(|index| (stable_usage[*index], source_usage[*index], *index));
        let mut needed = new_colors.len() - free_count;
        for index in releasable {
            if needed == 0 {
                break;
            }
            let color = colors[index];
            let count = protected_per_color.get_mut(&color).unwrap();
            if *count > 1 {
                protected[index] = false;
                *count -= 1;
                needed -= 1;
            }
        }
        debug_assert_eq!(needed, 0);
    }

    let mut available = (0..capacity)
        .filter(|index| !protected[*index])
        .collect::<Vec<_>>();
    available.sort_by_key(|index| (source_usage[*index], stable_usage[*index], *index));
    for (color, index) in new_colors.into_iter().zip(available) {
        colors[index] = color;
        protected[index] = true;
    }
    (colors, true)
}

fn arrange_palette_near_source(
    quantized: &[[u8; 4]],
    source: &[[u8; 4]],
) -> Vec<[u8; 4]> {
    let mut output = source.to_vec();
    let mut free = vec![true; source.len()];
    for &color in quantized {
        let index = source
            .iter()
            .enumerate()
            .filter(|(index, _)| free[*index])
            .min_by_key(|(_, candidate)| palette_color_distance(color, **candidate))
            .map(|(index, _)| index)
            .unwrap_or(0);
        output[index] = color;
        free[index] = false;
    }
    output
}

#[derive(Clone)]
struct PaletteBox {
    colors: Vec<([u8; 4], usize)>,
}

fn median_cut_palette(
    histogram: &BTreeMap<[u8; 4], usize>,
    capacity: usize,
) -> Vec<[u8; 4]> {
    let mut boxes = vec![PaletteBox {
        colors: histogram.iter().map(|(color, count)| (*color, *count)).collect(),
    }];
    while boxes.len() < capacity {
        let Some((box_index, channel)) = boxes
            .iter()
            .enumerate()
            .filter(|(_, palette_box)| palette_box.colors.len() > 1)
            .map(|(index, palette_box)| {
                let (channel, range) = palette_box_split_channel(palette_box);
                let weight = palette_box.colors.iter().map(|(_, count)| *count as u64).sum::<u64>();
                (index, channel, u64::from(range) * weight)
            })
            .max_by_key(|(_, _, score)| *score)
            .map(|(index, channel, _)| (index, channel))
        else {
            break;
        };
        let mut palette_box = boxes.swap_remove(box_index);
        palette_box.colors.sort_by_key(|(color, _)| (color[channel], *color));
        let total = palette_box.colors.iter().map(|(_, count)| *count).sum::<usize>();
        let mut accumulated = 0usize;
        let mut split = 1usize;
        for (index, (_, count)) in palette_box.colors.iter().enumerate() {
            accumulated += *count;
            if accumulated * 2 >= total {
                split = (index + 1).clamp(1, palette_box.colors.len() - 1);
                break;
            }
        }
        let right = palette_box.colors.split_off(split);
        boxes.push(PaletteBox {
            colors: palette_box.colors,
        });
        boxes.push(PaletteBox { colors: right });
    }
    boxes
        .iter()
        .map(palette_box_average)
        .collect::<Vec<_>>()
}

fn palette_box_split_channel(palette_box: &PaletteBox) -> (usize, u8) {
    let mut minimum = [u8::MAX; 4];
    let mut maximum = [u8::MIN; 4];
    for (color, _) in &palette_box.colors {
        for channel in 0..4 {
            minimum[channel] = minimum[channel].min(color[channel]);
            maximum[channel] = maximum[channel].max(color[channel]);
        }
    }
    (0..4)
        .map(|channel| (channel, maximum[channel] - minimum[channel]))
        .max_by_key(|(_, range)| *range)
        .unwrap_or((0, 0))
}

fn palette_box_average(palette_box: &PaletteBox) -> [u8; 4] {
    let total = palette_box.colors.iter().map(|(_, count)| *count as u64).sum::<u64>();
    let mut sums = [0u64; 4];
    for (color, count) in &palette_box.colors {
        for channel in 0..4 {
            sums[channel] += u64::from(color[channel]) * *count as u64;
        }
    }
    let mut output = [0u8; 4];
    for channel in 0..4 {
        output[channel] = ((sums[channel] + total / 2) / total) as u8;
    }
    output
}

fn select_palette_index(
    pixel: [u8; 4],
    palette: &[[u8; 4]],
    source_index: usize,
) -> usize {
    if palette.get(source_index) == Some(&pixel) {
        return source_index;
    }
    if let Some(index) = palette.iter().position(|candidate| *candidate == pixel) {
        return index;
    }
    palette
        .iter()
        .enumerate()
        .min_by_key(|(index, candidate)| {
            (
                palette_color_distance(pixel, **candidate),
                if *index == source_index { 0usize } else { 1usize },
                *index,
            )
        })
        .map(|(index, _)| index)
        .unwrap_or(0)
}

fn palette_color_distance(a: [u8; 4], b: [u8; 4]) -> u64 {
    let dr = i64::from(a[0]) - i64::from(b[0]);
    let dg = i64::from(a[1]) - i64::from(b[1]);
    let db = i64::from(a[2]) - i64::from(b[2]);
    let da = i64::from(a[3]) - i64::from(b[3]);
    (dr * dr + dg * dg + db * db + da * da) as u64
}

fn texture_visual_error(
    observed: &RgbaImage,
    target: &RgbaImage,
    format: u32,
) -> Result<u128, AssetError> {
    if observed.dimensions() != target.dimensions() {
        return Err(AssetError::InvalidProject(format!(
            "texture verification dimensions differ: observed {:?}, target {:?}",
            observed.dimensions(),
            target.dimensions()
        )));
    }
    let ignore_alpha = matches!(
        format & FORMAT_BASE_MASK,
        FORMAT_BC1 | FORMAT_BC2 | FORMAT_BC3
    ) && format & FORMAT_SWIZZLE_MASK == 0x4000;
    let mut error = 0u128;
    for (left, right) in observed.pixels().zip(target.pixels()) {
        let channels = if ignore_alpha { 3 } else { 4 };
        for channel in 0..channels {
            let delta = i128::from(left.0[channel]) - i128::from(right.0[channel]);
            error = error.saturating_add((delta * delta) as u128);
        }
    }
    Ok(error)
}

fn validate_palette(format: u32, palette: &EnginePaletteDocument) -> Result<(), AssetError> {
    if !matches!(
        format & FORMAT_SWIZZLE_MASK,
        0x0000 | 0x1000 | 0x2000 | 0x3000
    ) {
        return Err(AssetError::InvalidFormat(format!(
            "unsupported palette channel swizzle {:#06x}",
            format & FORMAT_SWIZZLE_MASK
        )));
    }
    let expected = match format & FORMAT_BASE_MASK {
        FORMAT_P4 => 16,
        FORMAT_P8 => 256,
        value => {
            return Err(AssetError::InvalidFormat(format!(
                "format {value:#010x} is not paletted"
            )))
        }
    };
    if palette.colors.len() != expected {
        return Err(AssetError::InvalidFormat(format!(
            "palette has {} colors, expected {expected}",
            palette.colors.len()
        )));
    }
    Ok(())
}

fn palette_index_at(input: &[u8], pixel: usize, format: u32, low_first: bool) -> usize {
    if format & FORMAT_BASE_MASK == FORMAT_P8 {
        return usize::from(input[pixel]);
    }
    let packed = input[pixel / 2];
    let low = pixel & 1 == 0;
    usize::from(if low == low_first {
        packed & 0x0f
    } else {
        packed >> 4
    })
}

fn parse_chunk_table(
    package: &[u8],
    chunk_region_offset: usize,
    table_offset: usize,
    stride: usize,
    maximum_decode_size: usize,
    profile: EnginePackageProfile,
) -> Result<ParsedChunkTable, AssetError> {
    let table_start = chunk_region_offset
        .checked_add(table_offset)
        .ok_or_else(|| AssetError::InvalidFormat("chunk table offset overflow".to_owned()))?;
    let count_word = read_u32(package, table_start)?;
    let count = profile.decode_chunk_count(count_word)?;
    let offsets_size = count
        .checked_add(1)
        .and_then(|value| value.checked_mul(4))
        .ok_or_else(|| AssetError::InvalidFormat("chunk offset table overflow".to_owned()))?;
    let header_size = 4usize
        .checked_add(offsets_size)
        .ok_or_else(|| AssetError::InvalidFormat("chunk header overflow".to_owned()))?;
    let mut offsets = Vec::with_capacity(count + 1);
    for index in 0..=count {
        offsets.push(
            usize::try_from(read_u32(package, table_start + 4 + index * 4)?).map_err(|_| {
                AssetError::InvalidFormat("chunk offset overflows usize".to_owned())
            })?,
        );
    }
    if offsets[0] < header_size || offsets.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(AssetError::InvalidFormat(
            "chunk block offsets are not strictly increasing".to_owned(),
        ));
    }
    let table_size = offsets[count];
    let table_end = table_start
        .checked_add(table_size)
        .ok_or_else(|| AssetError::InvalidFormat("chunk table end overflow".to_owned()))?;
    if table_end > package.len() {
        return Err(AssetError::InvalidFormat(format!(
            "truncated chunk table: needs {table_end:#x}, package has {:#x}",
            package.len()
        )));
    }

    let mut decoded_chunks = Vec::with_capacity(count);
    let mut ranges = Vec::with_capacity(count);
    let mut logical_end = 0usize;
    let mut chunks = Vec::with_capacity(count);
    for index in 0..count {
        let start = table_start + offsets[index];
        let end = table_start + offsets[index + 1];
        if end.saturating_sub(start) < 0x12 {
            return Err(AssetError::InvalidFormat(format!(
                "chunk {index} is smaller than its 0x10-byte header plus GZIP magic"
            )));
        }
        let expected = usize::try_from(read_u32(package, start)?).map_err(|_| {
            AssetError::InvalidFormat("chunk output size overflows usize".to_owned())
        })?;
        if expected == 0 {
            return Err(AssetError::InvalidFormat(format!(
                "chunk {index} declares zero output bytes"
            )));
        }
        if package.get(start + 0x10..start + 0x12) != Some(&[0x1f, 0x8b]) {
            return Err(AssetError::InvalidFormat(format!(
                "chunk {index} payload at +0x10 is not RFC1952 GZIP"
            )));
        }
        let mut decoder = GzDecoder::new(&package[start + 0x10..end]);
        let mut decoded = Vec::with_capacity(expected);
        decoder.read_to_end(&mut decoded)?;
        if decoded.len() != expected {
            return Err(AssetError::InvalidFormat(format!(
                "GZIP chunk {index} expands to {}, header declares {expected}",
                decoded.len()
            )));
        }
        let destination = index.checked_mul(stride).ok_or_else(|| {
            AssetError::InvalidFormat("chunk destination overflow".to_owned())
        })?;
        let destination_end = destination.checked_add(expected).ok_or_else(|| {
            AssetError::InvalidFormat("chunk destination size overflow".to_owned())
        })?;
        if destination_end > maximum_decode_size {
            return Err(AssetError::InvalidFormat(format!(
                "chunk {index} reaches decoded offset {destination_end:#x}, beyond caller buffer {maximum_decode_size:#x}"
            )));
        }
        ranges.push((destination, destination_end));
        logical_end = logical_end.max(destination_end);
        decoded_chunks.push((destination, decoded));
        chunks.push(EngineChunkDocument {
            output_size: u32::try_from(expected).unwrap(),
            reserved_words: [
                read_u32(package, start + 0x04)?,
                read_u32(package, start + 0x08)?,
                read_u32(package, start + 0x0c)?,
            ],
        });
    }
    let mut ordered = ranges.clone();
    ordered.sort_by_key(|range| range.0);
    if ordered.windows(2).any(|pair| pair[0].1 > pair[1].0) {
        return Err(AssetError::InvalidFormat(
            "chunk destination ranges overlap; engine permits ordered overwrite, but editable reconstruction would be lossy"
                .to_owned(),
        ));
    }
    let mut decompressed = vec![0u8; logical_end];
    for (destination, decoded) in decoded_chunks {
        decompressed[destination..destination + decoded.len()].copy_from_slice(&decoded);
    }
    Ok(ParsedChunkTable {
        first_block_offset: u32::try_from(offsets[0]).unwrap(),
        count_word,
        table_prefix: package[table_start..table_start + offsets[0]].to_vec(),
        table_size,
        chunks,
        decompressed,
        ranges,
    })
}

fn validate_visible_coverage(
    ranges: &[(usize, usize)],
    visible_size: usize,
) -> Result<(), AssetError> {
    let mut ordered = ranges.to_vec();
    ordered.sort_by_key(|range| range.0);
    let mut covered = 0usize;
    for (start, end) in ordered {
        if start > covered && covered < visible_size {
            return Err(AssetError::InvalidFormat(format!(
                "decoded texture has an uninitialized gap {covered:#x}..{:#x}",
                start.min(visible_size)
            )));
        }
        if start <= covered {
            covered = covered.max(end);
        }
        if covered >= visible_size {
            return Ok(());
        }
    }
    Err(AssetError::InvalidFormat(format!(
        "decoded chunks cover only {covered:#x} of {visible_size:#x} visible texture bytes"
    )))
}

fn validate_chunk_plan(
    chunks: &[EngineChunkDocument],
    stride: usize,
    decoded_size: usize,
) -> Result<(), AssetError> {
    if chunks.is_empty() || chunks.len() > MAX_CHUNKS {
        return Err(AssetError::InvalidProject(
            "chunk plan must contain 1..=255 chunks".to_owned(),
        ));
    }
    let mut ranges = Vec::with_capacity(chunks.len());
    let mut logical_end = 0usize;
    for (index, chunk) in chunks.iter().enumerate() {
        let size = usize::try_from(chunk.output_size).map_err(|_| {
            AssetError::InvalidProject("chunk output size overflows usize".to_owned())
        })?;
        if size == 0 {
            return Err(AssetError::InvalidProject(format!(
                "chunk {index} has zero output size"
            )));
        }
        let destination = index.checked_mul(stride).ok_or_else(|| {
            AssetError::InvalidProject("chunk destination overflow".to_owned())
        })?;
        let end = destination.checked_add(size).ok_or_else(|| {
            AssetError::InvalidProject("chunk range overflow".to_owned())
        })?;
        ranges.push((destination, end));
        logical_end = logical_end.max(end);
    }
    let mut ordered = ranges;
    ordered.sort_by_key(|range| range.0);
    if ordered.windows(2).any(|pair| pair[0].1 > pair[1].0) {
        return Err(AssetError::InvalidProject(
            "editable chunk plan contains overlapping destination writes".to_owned(),
        ));
    }
    if logical_end != decoded_size {
        return Err(AssetError::InvalidProject(format!(
            "chunk plan reaches {logical_end} bytes, decoded buffer has {decoded_size}"
        )));
    }
    Ok(())
}

fn build_chunk_table_incremental(
    profile: EnginePackageProfile,
    decoded: &[u8],
    source_decoded: &[u8],
    source_table: &[u8],
    chunks: &[EngineChunkDocument],
    stride: usize,
    first_block_offset: usize,
    count_word: u32,
    table_prefix: &[u8],
) -> Result<Vec<u8>, AssetError> {
    validate_chunk_plan(chunks, stride, decoded.len())?;
    if source_decoded.len() != decoded.len() {
        return Err(AssetError::InvalidProject(
            "source and edited decoded buffers differ in length".to_owned(),
        ));
    }
    if source_table.len() < first_block_offset || table_prefix.len() != first_block_offset {
        return Err(AssetError::InvalidProject(
            "source chunk table is smaller than its preserved prefix".to_owned(),
        ));
    }
    let source_count_word = read_u32(source_table, 0)
        .map_err(|error| AssetError::InvalidProject(error.to_string()))?;
    if source_count_word != count_word {
        return Err(AssetError::InvalidProject(
            "source chunk-table count word differs from package.json".to_owned(),
        ));
    }
    let source_count = profile
        .decode_chunk_count(source_count_word)
        .map_err(|error| AssetError::InvalidProject(error.to_string()))?;
    if source_count != chunks.len() {
        return Err(AssetError::InvalidProject(format!(
            "source chunk table contains {source_count} chunks, project contains {}",
            chunks.len()
        )));
    }
    let mut source_offsets = Vec::with_capacity(chunks.len() + 1);
    for index in 0..=chunks.len() {
        source_offsets.push(
            usize::try_from(
                read_u32(source_table, 4 + index * 4)
                    .map_err(|error| AssetError::InvalidProject(error.to_string()))?,
            )
            .map_err(|_| AssetError::InvalidProject("source chunk offset overflows usize".to_owned()))?,
        );
    }
    if source_offsets[0] != first_block_offset
        || source_offsets.windows(2).any(|pair| pair[0] >= pair[1])
        || source_offsets.last().copied() != Some(source_table.len())
        || source_table[..first_block_offset] != table_prefix[..]
    {
        return Err(AssetError::InvalidProject(
            "source chunk table offsets/prefix are inconsistent".to_owned(),
        ));
    }

    let mut output = table_prefix.to_vec();
    let encoded_count = profile.encode_chunk_count(count_word, chunks.len())?;
    output[0..4].copy_from_slice(&encoded_count.to_le_bytes());
    let mut block_offset = first_block_offset;
    for (index, chunk) in chunks.iter().enumerate() {
        let location = 4 + index * 4;
        output[location..location + 4].copy_from_slice(
            &u32::try_from(block_offset)
                .map_err(|_| AssetError::InvalidProject("chunk table exceeds u32".to_owned()))?
                .to_le_bytes(),
        );
        let size = usize::try_from(chunk.output_size).map_err(|_| {
            AssetError::InvalidProject("chunk output size overflows usize".to_owned())
        })?;
        let destination = index.checked_mul(stride).ok_or_else(|| {
            AssetError::InvalidProject("chunk destination overflow".to_owned())
        })?;
        let edited = decoded.get(destination..destination + size).ok_or_else(|| {
            AssetError::InvalidProject(format!(
                "chunk {index} edited source range exceeds decoded buffer"
            ))
        })?;
        let original = source_decoded
            .get(destination..destination + size)
            .ok_or_else(|| {
                AssetError::InvalidProject(format!(
                    "chunk {index} original source range exceeds decoded buffer"
                ))
            })?;
        if edited == original {
            let start = source_offsets[index];
            let end = source_offsets[index + 1];
            output.extend_from_slice(&source_table[start..end]);
        } else {
            append_encoded_chunk(&mut output, edited, chunk)?;
        }
        block_offset = output.len();
    }
    let final_location = 4 + chunks.len() * 4;
    output[final_location..final_location + 4].copy_from_slice(
        &u32::try_from(block_offset)
            .map_err(|_| AssetError::InvalidProject("chunk table exceeds u32".to_owned()))?
            .to_le_bytes(),
    );
    Ok(output)
}

fn append_encoded_chunk(
    output: &mut Vec<u8>,
    source: &[u8],
    chunk: &EngineChunkDocument,
) -> Result<(), AssetError> {
    let mut encoder = GzBuilder::new()
        .mtime(0)
        .write(Vec::new(), Compression::best());
    encoder.write_all(source)?;
    let gzip = encoder.finish()?;
    output.extend_from_slice(&chunk.output_size.to_le_bytes());
    for word in chunk.reserved_words {
        output.extend_from_slice(&word.to_le_bytes());
    }
    output.extend_from_slice(&gzip);
    while output.len() % 4 != 0 {
        output.push(0);
    }
    Ok(())
}

#[cfg(test)]
fn build_chunk_table(
    profile: EnginePackageProfile,
    decoded: &[u8],
    chunks: &[EngineChunkDocument],
    stride: usize,
    first_block_offset: usize,
    count_word: u32,
    table_prefix: &[u8],
) -> Result<Vec<u8>, AssetError> {
    validate_chunk_plan(chunks, stride, decoded.len())?;
    let minimum_header = 4usize
        .checked_add((chunks.len() + 1).checked_mul(4).ok_or_else(|| {
            AssetError::InvalidProject("chunk offset header overflow".to_owned())
        })?)
        .ok_or_else(|| AssetError::InvalidProject("chunk table header overflow".to_owned()))?;
    if first_block_offset < minimum_header {
        return Err(AssetError::InvalidProject(format!(
            "first chunk block offset {first_block_offset} is smaller than header {minimum_header}"
        )));
    }
    if table_prefix.len() != first_block_offset {
        return Err(AssetError::InvalidProject(
            "preserved chunk-table prefix length differs from first block offset".to_owned(),
        ));
    }
    let mut output = table_prefix.to_vec();
    let encoded_count = profile.encode_chunk_count(count_word, chunks.len())?;
    output[0..4].copy_from_slice(&encoded_count.to_le_bytes());
    let mut block_offset = first_block_offset;
    for (index, chunk) in chunks.iter().enumerate() {
        let location = 4 + index * 4;
        output[location..location + 4].copy_from_slice(
            &u32::try_from(block_offset)
                .map_err(|_| AssetError::InvalidProject("chunk table exceeds u32".to_owned()))?
                .to_le_bytes(),
        );
        let size = usize::try_from(chunk.output_size).map_err(|_| {
            AssetError::InvalidProject("chunk output size overflows usize".to_owned())
        })?;
        let destination = index.checked_mul(stride).ok_or_else(|| {
            AssetError::InvalidProject("chunk destination overflow".to_owned())
        })?;
        let source = decoded.get(destination..destination + size).ok_or_else(|| {
            AssetError::InvalidProject(format!(
                "chunk {index} source range exceeds decoded buffer"
            ))
        })?;
        let mut encoder = GzBuilder::new()
            .mtime(0)
            .write(Vec::new(), Compression::best());
        encoder.write_all(source)?;
        let gzip = encoder.finish()?;
        output.extend_from_slice(&chunk.output_size.to_le_bytes());
        for word in chunk.reserved_words {
            output.extend_from_slice(&word.to_le_bytes());
        }
        output.extend_from_slice(&gzip);
        while output.len() % 4 != 0 {
            output.push(0);
        }
        block_offset = output.len();
    }
    let final_location = 4 + chunks.len() * 4;
    output[final_location..final_location + 4].copy_from_slice(
        &u32::try_from(block_offset)
            .map_err(|_| AssetError::InvalidProject("chunk table exceeds u32".to_owned()))?
            .to_le_bytes(),
    );
    Ok(output)
}

fn parse_bundle(input: &[u8]) -> Result<ParsedBundle, AssetError> {
    if input.len() < 8 {
        return Err(AssetError::InvalidFormat("not an image bundle".to_owned()));
    }
    let count = usize::from(input[0]);
    if count == 0 {
        return Err(AssetError::InvalidFormat("image bundle count is zero".to_owned()));
    }
    let table_end = 4usize
        .checked_add(count.checked_mul(4).ok_or_else(|| {
            AssetError::InvalidFormat("bundle table overflow".to_owned())
        })?)
        .ok_or_else(|| AssetError::InvalidFormat("bundle header overflow".to_owned()))?;
    if table_end > input.len() {
        return Err(AssetError::InvalidFormat("truncated image bundle".to_owned()));
    }
    let mut logical_offsets = Vec::with_capacity(count);
    for index in 0..count {
        let offset = usize::try_from(read_u32(input, 4 + index * 4)?).map_err(|_| {
            AssetError::InvalidFormat("bundle package offset overflows usize".to_owned())
        })?;
        if offset < table_end || offset >= input.len() {
            return Err(AssetError::InvalidFormat(format!(
                "bundle package {index} offset {offset:#x} is outside {table_end:#x}..{:#x}",
                input.len()
            )));
        }
        logical_offsets.push(offset);
    }

    let mut unique = logical_offsets.clone();
    unique.sort_unstable();
    unique.dedup();
    let physical_by_offset = unique
        .iter()
        .copied()
        .enumerate()
        .map(|(index, offset)| (offset, u32::try_from(index).unwrap()))
        .collect::<BTreeMap<_, _>>();
    let logical_to_physical = logical_offsets
        .iter()
        .map(|offset| physical_by_offset[offset])
        .collect::<Vec<_>>();
    let mut ranges = Vec::with_capacity(unique.len());
    for (index, start) in unique.iter().copied().enumerate() {
        let end = unique.get(index + 1).copied().unwrap_or(input.len());
        if start >= end {
            return Err(AssetError::InvalidFormat(
                "bundle physical package has an empty range".to_owned(),
            ));
        }
        ranges.push((start, end));
    }
    Ok(ParsedBundle {
        prefix: input[..unique[0]].to_vec(),
        ranges,
        logical_to_physical,
    })
}

fn infer_bundle_alignment(ranges: &[(usize, usize)]) -> u32 {
    let gcd = ranges
        .iter()
        .map(|(start, _)| *start)
        .filter(|start| *start != 0)
        .reduce(greatest_common_divisor)
        .unwrap_or(4);
    let power_of_two_divisor = 1usize << gcd.trailing_zeros();
    u32::try_from(power_of_two_divisor.clamp(4, 0x1000)).unwrap_or(4)
}

fn greatest_common_divisor(mut a: usize, mut b: usize) -> usize {
    while b != 0 {
        let remainder = a % b;
        a = b;
        b = remainder;
    }
    a
}

fn profile_name(profile: EnginePackageProfile) -> &'static str {
    match profile {
        EnginePackageProfile::AddPt => "addpt.cpk",
        EnginePackageProfile::Bk => "bk.cpk",
        EnginePackageProfile::Bsf => "bsf.cpk",
        EnginePackageProfile::Pt => "pt.cpk",
    }
}

fn fnv1a64_hex(bytes: &[u8]) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
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
            "hex field has odd length".to_owned(),
        ));
    }
    let mut output = Vec::with_capacity(value.len() / 2);
    let bytes = value.as_bytes();
    for index in (0..bytes.len()).step_by(2) {
        let high = hex_nibble(bytes[index]).ok_or_else(|| {
            AssetError::InvalidProject(format!("invalid hex character at index {index}"))
        })?;
        let low = hex_nibble(bytes[index + 1]).ok_or_else(|| {
            AssetError::InvalidProject(format!("invalid hex character at index {}", index + 1))
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


pub(crate) fn compute_engine_integrity_footer(
    input: &[u8],
) -> Result<[u8; ENGINE_INTEGRITY_FOOTER_SIZE], String> {
    if input.len() % 0x10 != 0 {
        return Err(format!(
            "integrity checksum input has {:#x} bytes, expected a multiple of 0x10",
            input.len()
        ));
    }

    // FUN_8102B4AC is installed as the common fixed-sector CPK read callback
    // at 0x8110C328. It treats each 16-byte block as two little-endian u64
    // lanes, seeds both with 0x1111111111111111, and compares the final sums
    // with the last 16 bytes of the allocation.
    let mut lane_0 = ENGINE_INTEGRITY_SEED;
    let mut lane_1 = ENGINE_INTEGRITY_SEED;
    for chunk in input.chunks_exact(0x10) {
        lane_0 = lane_0.wrapping_add(u64::from_le_bytes(
            chunk[..8]
                .try_into()
                .expect("16-byte checksum chunk always contains first u64"),
        ));
        lane_1 = lane_1.wrapping_add(u64::from_le_bytes(
            chunk[8..]
                .try_into()
                .expect("16-byte checksum chunk always contains second u64"),
        ));
    }

    let mut footer = [0u8; ENGINE_INTEGRITY_FOOTER_SIZE];
    footer[..8].copy_from_slice(&lane_0.to_le_bytes());
    footer[8..].copy_from_slice(&lane_1.to_le_bytes());
    Ok(footer)
}

pub(crate) fn regenerate_engine_integrity_footer(output: &mut [u8]) -> Result<(), String> {
    if output.len() < ENGINE_INTEGRITY_FOOTER_SIZE {
        return Err(format!(
            "allocation has {:#x} bytes, shorter than the 16-byte integrity footer",
            output.len()
        ));
    }
    let footer_start = output.len() - ENGINE_INTEGRITY_FOOTER_SIZE;
    let footer = compute_engine_integrity_footer(&output[..footer_start])?;
    output[footer_start..].copy_from_slice(&footer);
    Ok(())
}

pub(crate) fn verify_engine_integrity_footer(input: &[u8]) -> Result<(), String> {
    if input.len() < ENGINE_INTEGRITY_FOOTER_SIZE {
        return Err(format!(
            "allocation has {:#x} bytes, shorter than the 16-byte integrity footer",
            input.len()
        ));
    }
    let footer_start = input.len() - ENGINE_INTEGRITY_FOOTER_SIZE;
    let expected = compute_engine_integrity_footer(&input[..footer_start])?;
    let stored = &input[footer_start..];
    if stored != expected.as_slice() {
        return Err(format!(
            "stored {}, expected {}",
            encode_hex(stored),
            encode_hex(&expected)
        ));
    }
    Ok(())
}

fn write_u32(output: &mut [u8], offset: usize, value: u32) -> Result<(), AssetError> {
    let target = output.get_mut(offset..offset + 4).ok_or_else(|| {
        AssetError::InvalidProject("package u32 write exceeds generated package layout".to_owned())
    })?;
    target.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn read_u16(input: &[u8], offset: usize) -> Result<u16, AssetError> {
    let bytes = input.get(offset..offset + 2).ok_or_else(|| {
        AssetError::InvalidFormat(format!("truncated u16 at {offset:#x}"))
    })?;
    Ok(u16::from_le_bytes(bytes.try_into().unwrap()))
}

fn read_u32(input: &[u8], offset: usize) -> Result<u32, AssetError> {
    let bytes = input.get(offset..offset + 4).ok_or_else(|| {
        AssetError::InvalidFormat(format!("truncated u32 at {offset:#x}"))
    })?;
    Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
}

fn align_up(value: usize, alignment: usize) -> Result<usize, AssetError> {
    if alignment == 0 || !alignment.is_power_of_two() {
        return Err(AssetError::InvalidProject(
            "alignment must be a non-zero power of two".to_owned(),
        ));
    }
    value
        .checked_add(alignment - 1)
        .map(|sum| sum & !(alignment - 1))
        .ok_or_else(|| AssetError::InvalidProject("alignment overflow".to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_integrity_footer_matches_two_seeded_u64_lanes() {
        let mut input = Vec::new();
        input.extend_from_slice(&1u64.to_le_bytes());
        input.extend_from_slice(&2u64.to_le_bytes());
        input.extend_from_slice(&3u64.to_le_bytes());
        input.extend_from_slice(&4u64.to_le_bytes());
        let footer = compute_engine_integrity_footer(&input).unwrap();
        assert_eq!(
            footer,
            [
                0x15, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
                0x17, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
            ]
        );
    }

    #[test]
    fn engine_integrity_footer_changes_after_texture_bytes_change() {
        let mut allocation = vec![0u8; 0x800];
        regenerate_engine_integrity_footer(&mut allocation).unwrap();
        let original = allocation[allocation.len() - ENGINE_INTEGRITY_FOOTER_SIZE..].to_vec();
        allocation[0x123] ^= 0x5a;
        assert!(verify_engine_integrity_footer(&allocation).is_err());
        regenerate_engine_integrity_footer(&mut allocation).unwrap();
        assert!(verify_engine_integrity_footer(&allocation).is_ok());
        assert_ne!(
            original,
            allocation[allocation.len() - ENGINE_INTEGRITY_FOOTER_SIZE..]
        );
    }

    #[test]
    fn bundle_allows_aliases_and_non_monotonic_logical_offsets() {
        let mut input = vec![0u8; 0x80];
        input[0] = 3;
        input[4..8].copy_from_slice(&0x40u32.to_le_bytes());
        input[8..12].copy_from_slice(&0x20u32.to_le_bytes());
        input[12..16].copy_from_slice(&0x40u32.to_le_bytes());
        let parsed = parse_bundle(&input).unwrap();
        assert_eq!(parsed.ranges, vec![(0x20, 0x40), (0x40, 0x80)]);
        assert_eq!(parsed.logical_to_physical, vec![1, 0, 1]);
        assert_eq!(parsed.prefix, input[..0x20]);
    }

    #[test]
    fn bundle_build_preserves_unknown_prefix_and_logical_aliases() {
        let mut prefix = (0u8..0x20).collect::<Vec<_>>();
        prefix[0] = 3;
        let document = EnginePackageDocument {
            document_version: DOCUMENT_VERSION,
            profile: EnginePackageProfile::Pt,
            entry_id: 0,
            allocation_size: 0x100,
            wrapped_bundle: true,
            bundle_prefix_hex: encode_hex(&prefix),
            bundle_alignment: 0x10,
            bundle_map: vec![1, 0, 1],
            subpackages: Vec::new(),
        };
        let packages = vec![vec![0xaau8; 5], vec![0xbbu8; 7]];
        let rebuilt = build_bundle(&document, &packages).unwrap();
        assert_eq!(rebuilt[0], 3);
        assert_eq!(&rebuilt[1..4], &prefix[1..4]);
        assert_eq!(&rebuilt[16..0x20], &prefix[16..0x20]);
        let parsed = parse_bundle(&rebuilt).unwrap();
        assert_eq!(parsed.logical_to_physical, vec![1, 0, 1]);
        assert_eq!(parsed.prefix[1..4], prefix[1..4]);
        assert_eq!(parsed.prefix[16..0x20], prefix[16..0x20]);
    }

    #[test]
    fn chunk_count_above_byte_range_is_rejected() {
        let mut package = vec![0u8; 0x1000];
        package[0..4].copy_from_slice(&0x100u32.to_le_bytes());
        let error = parse_chunk_table(
            &package,
            0,
            0,
            0x100,
            0x100000,
            EnginePackageProfile::Bk,
        )
        .err()
        .unwrap();
        assert!(error.to_string().contains("one byte"));
    }

    #[test]
    fn zero_count_is_engine_encoded_single_chunk() {
        for profile in [
            EnginePackageProfile::AddPt,
            EnginePackageProfile::Bk,
            EnginePackageProfile::Bsf,
            EnginePackageProfile::Pt,
        ] {
            assert_eq!(profile.decode_chunk_count(0).unwrap(), 1);
            assert_eq!(profile.encode_chunk_count(0, 1).unwrap(), 0);
        }
    }

    #[test]
    fn runtime_bsf_profile_matches_engine_partition() {
        assert_eq!(
            EnginePackageProfile::Bsf.maximum_decode_size(0x99).unwrap(),
            0x320000
        );
        assert_eq!(
            EnginePackageProfile::Bsf.maximum_decode_size(0x9a).unwrap(),
            0x300000
        );
    }

    #[test]
    fn bsf_count_uses_low_byte_and_preserves_unknown_upper_bytes() {
        let original = 0xa5_5a_c3_03u32;
        assert_eq!(
            EnginePackageProfile::Bsf.decode_chunk_count(original).unwrap(),
            3
        );
        assert_eq!(
            EnginePackageProfile::Bsf
                .encode_chunk_count(original, 2)
                .unwrap(),
            0xa5_5a_c3_02
        );
    }

    #[test]
    fn bk_header_does_not_dereference_embedded_descriptor() {
        let mut package = vec![0u8; 0x2000];
        package[0x04..0x08].copy_from_slice(&0xffff_fff0u32.to_le_bytes());
        package[0x34..0x38].copy_from_slice(&0u32.to_le_bytes());
        package[0x3c..0x40].copy_from_slice(&0x220000u32.to_le_bytes());
        assert!(validate_single_header(&package, EnginePackageProfile::Bk).is_ok());
        assert!(validate_single_header(&package, EnginePackageProfile::Bsf).is_err());
    }

    #[test]
    fn metadata_count_cannot_exceed_engine_pool() {
        let mut package = vec![0u8; 0x2000];
        package[0x04..0x08].copy_from_slice(&0x40u32.to_le_bytes());
        package[0x10..0x14].copy_from_slice(&0x400u32.to_le_bytes());
        package[0x14..0x18].copy_from_slice(&0x80u32.to_le_bytes());
        package[0x34..0x38].copy_from_slice(&0x100u32.to_le_bytes());
        let error = validate_single_header(&package, EnginePackageProfile::AddPt)
            .err()
            .unwrap();
        assert!(error.to_string().contains("<0x400-record pool"));
    }

    #[test]
    fn incremental_table_reuses_only_unchanged_raw_blocks() {
        let chunks = vec![
            EngineChunkDocument { output_size: 4, reserved_words: [1, 2, 3] },
            EngineChunkDocument { output_size: 4, reserved_words: [4, 5, 6] },
        ];
        let prefix = vec![0u8; 16];
        let source_decoded = vec![1, 2, 3, 4, 5, 6, 7, 8];
        let source_table = build_chunk_table(
            EnginePackageProfile::Bk,
            &source_decoded,
            &chunks,
            4,
            16,
            2,
            &prefix,
        )
        .unwrap();
        let mut edited = source_decoded.clone();
        edited[6] = 99;
        let rebuilt = build_chunk_table_incremental(
            EnginePackageProfile::Bk,
            &edited,
            &source_decoded,
            &source_table,
            &chunks,
            4,
            16,
            2,
            &prefix,
        )
        .unwrap();
        let source_first = usize::try_from(read_u32(&source_table, 4).unwrap()).unwrap();
        let source_second = usize::try_from(read_u32(&source_table, 8).unwrap()).unwrap();
        let rebuilt_first = usize::try_from(read_u32(&rebuilt, 4).unwrap()).unwrap();
        let rebuilt_second = usize::try_from(read_u32(&rebuilt, 8).unwrap()).unwrap();
        assert_eq!(
            &rebuilt[rebuilt_first..rebuilt_second],
            &source_table[source_first..source_second]
        );
        let parsed = parse_chunk_table(
            &rebuilt,
            0,
            0,
            4,
            0x100,
            EnginePackageProfile::Bk,
        )
        .unwrap();
        assert_eq!(parsed.decompressed, edited);
    }

    #[test]
    fn changed_bc_texture_is_reencoded_without_copy_only_gate() {
        let image = RgbaImage::from_pixel(4, 4, Rgba([255, 0, 0, 255]));
        let layout = EngineTextureLayout {
            palette_index: u32::MAX,
            flags: 0,
            texture_type: TEXTURE_TYPE_LINEAR,
            format: FORMAT_BC1,
            width: 4,
            height: 4,
            mip_count: 0,
        };
        let encoded = encode_package_texture(&image, layout, None, 8, &[0u8; 8]).unwrap();
        assert_eq!(encoded.bytes.len(), 8);
        assert_ne!(encoded.bytes, vec![0u8; 8]);
        assert!(!encoded.exact_visual_roundtrip);
    }

    #[test]
    fn paletted_encode_preserves_duplicate_source_index() {
        let mut colors = vec![[0u8, 0, 0, 255]; 16];
        colors[3] = [10, 20, 30, 255];
        colors[7] = [10, 20, 30, 255];
        let palette = EnginePaletteDocument {
            offset: 0,
            low_nibble_first: true,
            colors,
        };
        let layout = EngineTextureLayout {
            palette_index: u32::MAX,
            flags: 0,
            texture_type: TEXTURE_TYPE_LINEAR,
            format: FORMAT_P4,
            width: 2,
            height: 1,
            mip_count: 0,
        };
        let image = RgbaImage::from_raw(
            2,
            1,
            vec![10, 20, 30, 255, 10, 20, 30, 255],
        )
        .unwrap();
        let encoded = encode_paletted_texture(&image, layout, &palette, 1, &[0x73]).unwrap();
        assert_eq!(encoded.bytes, vec![0x73]);
        assert!(encoded.exact_visual_roundtrip);
    }

    #[test]
    fn paletted_new_color_keeps_used_duplicate_indices_when_space_exists() {
        let mut colors = vec![[0u8, 0, 0, 255]; 16];
        colors[3] = [10, 20, 30, 255];
        colors[7] = [10, 20, 30, 255];
        let palette = EnginePaletteDocument {
            offset: 0,
            low_nibble_first: true,
            colors,
        };
        let layout = EngineTextureLayout {
            palette_index: u32::MAX,
            flags: 0,
            texture_type: TEXTURE_TYPE_LINEAR,
            format: FORMAT_P4,
            width: 3,
            height: 1,
            mip_count: 0,
        };
        let image = RgbaImage::from_raw(
            3,
            1,
            vec![
                10, 20, 30, 255,
                10, 20, 30, 255,
                1, 2, 3, 255,
            ],
        )
        .unwrap();
        let encoded = encode_paletted_texture(&image, layout, &palette, 2, &[0x73, 0xa0]).unwrap();
        assert_eq!(encoded.bytes[0], 0x73);
        assert_eq!(encoded.bytes[1] & 0xf0, 0xa0);
        assert_eq!(
            encoded.palette.as_ref().unwrap().colors[usize::from(encoded.bytes[1] & 0x0f)],
            [1, 2, 3, 255]
        );
    }

    #[test]
    fn paletted_encode_adds_new_color_without_manual_json_edit() {
        let palette = EnginePaletteDocument {
            offset: 0,
            low_nibble_first: true,
            colors: vec![[0u8, 0, 0, 255]; 16],
        };
        let layout = EngineTextureLayout {
            palette_index: u32::MAX,
            flags: 0,
            texture_type: TEXTURE_TYPE_LINEAR,
            format: FORMAT_P4,
            width: 1,
            height: 1,
            mip_count: 0,
        };
        let image = RgbaImage::from_pixel(1, 1, Rgba([1, 2, 3, 255]));
        let encoded = encode_paletted_texture(&image, layout, &palette, 1, &[0]).unwrap();
        let rebuilt_palette = encoded.palette.unwrap();
        let index = usize::from(encoded.bytes[0] & 0x0f);
        assert_eq!(rebuilt_palette.colors[index], [1, 2, 3, 255]);
        assert!(encoded.exact_visual_roundtrip);
    }

    #[test]
    fn p4_odd_pixel_count_preserves_unused_source_nibble() {
        let palette = EnginePaletteDocument {
            offset: 0,
            low_nibble_first: true,
            colors: (0u8..16).map(|value| [value, value, value, 255]).collect(),
        };
        let layout = EngineTextureLayout {
            palette_index: u32::MAX,
            flags: 0,
            texture_type: TEXTURE_TYPE_LINEAR,
            format: FORMAT_P4,
            width: 1,
            height: 1,
            mip_count: 0,
        };
        let image = RgbaImage::from_pixel(1, 1, Rgba([3, 3, 3, 255]));
        let encoded = encode_paletted_texture(&image, layout, &palette, 1, &[0xa3]).unwrap();
        assert_eq!(encoded.bytes[0] & 0xf0, 0xa0);
        assert_eq!(encoded.bytes[0] & 0x0f, 3);
    }

    #[test]
    fn paletted_encode_quantizes_when_color_count_exceeds_capacity() {
        let palette = EnginePaletteDocument {
            offset: 0,
            low_nibble_first: true,
            colors: vec![[0u8, 0, 0, 255]; 16],
        };
        let layout = EngineTextureLayout {
            palette_index: u32::MAX,
            flags: 0,
            texture_type: TEXTURE_TYPE_LINEAR,
            format: FORMAT_P4,
            width: 17,
            height: 1,
            mip_count: 0,
        };
        let mut raw = Vec::new();
        for value in 0u8..17 {
            raw.extend_from_slice(&[value * 10, value * 7, value * 3, 255]);
        }
        let image = RgbaImage::from_raw(17, 1, raw).unwrap();
        let encoded = encode_paletted_texture(&image, layout, &palette, 9, &[0u8; 9]).unwrap();
        assert_eq!(encoded.bytes.len(), 9);
        assert_eq!(encoded.palette.unwrap().colors.len(), 16);
        assert!(!encoded.exact_visual_roundtrip);
    }
}
