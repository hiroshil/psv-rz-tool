use std::fs;
use std::path::Path;

use crate::engine_allocations::{SC_METADATA, SC_SECTORS, SECTOR_SIZE};
use crate::error::AssetError;

pub const SC_ENTRY_COUNT: usize = 89;
pub const LT_STOCK_GLYPH_COUNT: u32 = 0x0e12;
const LT_GLYPH_BYTES: u32 = 0x120;
const LT_LOAD_SIZE_VA: u32 = 0x8101_b510;
const LT_SIZE_TABLE_VA: u32 = 0x8112_9e5c;
const LT_COUNT_TABLE_VA: u32 = 0x8112_9e60;
const LT_STOCK_ALLOCATION_SIZE: u32 = 0x000f_d800;
const LT_STOCK_SECTOR_COUNT: u32 = 0x01fb;
const LT_RUNTIME_GLYPH_LIMIT_SITES: &[(u32, u8)] = &[
    (0x8100_140e, 0),
    (0x8102_d37c, 1),
    (0x8104_dc38, 0),
];
const SC_SECTOR_TABLE_VA: u32 = 0x8111_344c;
const SC_METADATA_TABLE_VA: u32 = 0x810f_9b1c;
const SC_BUFFER_MOV_VA: u32 = 0x8101_b554;
const VWF_RUNTIME_HASH_RANGE_START_VA: u32 = 0x8100_0000;
const VWF_RUNTIME_HASH_RANGE_END_VA: u32 = 0x8110_0000;
const VWF_RUNTIME_HASH_RANGE_SIZE: usize =
    (VWF_RUNTIME_HASH_RANGE_END_VA - VWF_RUNTIME_HASH_RANGE_START_VA) as usize;
const VWF_RUNTIME_HASH_RANGE_SHA256: &str =
    "c7cc66521264acdc6d259ad189d1a6fe731901855a6708ae3a3b036334091a84";

#[derive(Debug, Clone, Copy, Default)]
pub struct ScEntryPatch {
    pub sectors: u16,
    pub stream_count: u16,
    pub primary_count: u16,
    pub secondary_count: u16,
}

#[derive(Debug, Clone)]
pub struct ScPatchPlan {
    pub entries: [ScEntryPatch; SC_ENTRY_COUNT],
}

#[derive(Debug, Clone, Copy)]
pub struct EbootPatchReport {
    pub changed: bool,
    pub script_buffer_size: u32,
    pub total_sectors: u16,
    pub forced_runtime_hash_mismatch: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct LtPatchPlan {
    pub glyph_count: u32,
    pub allocation_size: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct LtEbootPatchReport {
    pub changed: bool,
    pub glyph_count: u32,
    pub allocation_size: u32,
    pub sector_count: u32,
    pub forced_runtime_hash_mismatch: bool,
}

impl ScPatchPlan {
    pub fn differs_from_stock(&self) -> bool {
        self.entries.iter().enumerate().any(|(index, entry)| {
            entry.sectors != SC_SECTORS[index]
                || entry.stream_count != SC_METADATA[index].stream_count
                || entry.primary_count != SC_METADATA[index].primary_count
                || entry.secondary_count != SC_METADATA[index].secondary_count
        })
    }

    pub fn max_allocation(&self) -> usize {
        self.entries
            .iter()
            .map(|entry| usize::from(entry.sectors) * SECTOR_SIZE)
            .max()
            .unwrap_or(0x20000)
    }
}


#[derive(Debug, Clone, serde::Serialize)]
pub struct ScAllocationMapDocument {
    pub format: String,
    pub version: u32,
    pub source_eboot_sha256: String,
    pub entries: Vec<ScAllocationEntry>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ScAllocationEntry {
    pub entry_id: u32,
    pub start_sector: u16,
    pub sector_count: u16,
    pub allocation_size: usize,
    pub allocation_size_hex: String,
    pub stock_sector_count: u16,
    pub stock_allocation_size: usize,
    pub stream_base: u16,
    pub stream_count: u16,
    pub primary_base: u16,
    pub primary_count: u16,
    pub secondary_base: u16,
    pub secondary_count: u16,
    pub patched: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct LtAllocationMapDocument {
    pub format: String,
    pub version: u32,
    pub source_eboot_sha256: String,
    pub glyph_count: u32,
    pub glyph_count_hex: String,
    pub allocation_size: usize,
    pub allocation_size_hex: String,
    pub sector_count: u32,
    pub stock_glyph_count: u32,
    pub stock_allocation_size: usize,
    pub stock_sector_count: u32,
    pub patched: bool,
}

pub fn extract_sc_allocation_map(input: &Path) -> Result<ScAllocationMapDocument, AssetError> {
    let bytes = fs::read(input)?;
    let elf = Elf32View::parse(&bytes)?;
    let sector_offset = elf.virtual_to_file_offset(SC_SECTOR_TABLE_VA, SC_ENTRY_COUNT * 4)?;
    let metadata_offset = elf.virtual_to_file_offset(SC_METADATA_TABLE_VA, SC_ENTRY_COUNT * 12)?;
    validate_existing_sector_table(&bytes[sector_offset..sector_offset + SC_ENTRY_COUNT * 4])?;
    validate_existing_metadata_table(&bytes[metadata_offset..metadata_offset + SC_ENTRY_COUNT * 12])?;
    let mut entries = Vec::with_capacity(SC_ENTRY_COUNT);
    for index in 0..SC_ENTRY_COUNT {
        let base = sector_offset + index * 4;
        let start_sector = u16::from_le_bytes([bytes[base], bytes[base + 1]]);
        let sector_count = u16::from_le_bytes([bytes[base + 2], bytes[base + 3]]);
        let allocation_size = usize::from(sector_count) * SECTOR_SIZE;
        let stock_sector_count = SC_SECTORS[index];
        let stock_allocation_size = usize::from(stock_sector_count) * SECTOR_SIZE;
        let metadata_base = metadata_offset + index * 12;
        let stream_base = read_u16(&bytes, metadata_base)?;
        let stream_count = read_u16(&bytes, metadata_base + 2)?;
        let primary_base = read_u16(&bytes, metadata_base + 4)?;
        let primary_count = read_u16(&bytes, metadata_base + 6)?;
        let secondary_base = read_u16(&bytes, metadata_base + 8)?;
        let secondary_count = read_u16(&bytes, metadata_base + 10)?;
        let stock_metadata = SC_METADATA[index];
        entries.push(ScAllocationEntry {
            entry_id: index as u32,
            start_sector,
            sector_count,
            allocation_size,
            allocation_size_hex: format!("{allocation_size:#x}"),
            stock_sector_count,
            stock_allocation_size,
            stream_base,
            stream_count,
            primary_base,
            primary_count,
            secondary_base,
            secondary_count,
            patched: sector_count != stock_sector_count
                || stream_base != stock_metadata.stream_base
                || stream_count != stock_metadata.stream_count
                || primary_base != stock_metadata.primary_base
                || primary_count != stock_metadata.primary_count
                || secondary_base != stock_metadata.secondary_base
                || secondary_count != stock_metadata.secondary_count,
        });
    }
    Ok(ScAllocationMapDocument {
        format: "rz-sc-allocation-map".to_owned(),
        version: 1,
        source_eboot_sha256: sha256_hex(&bytes),
        entries,
    })
}

pub fn extract_lt_allocation_map(input: &Path) -> Result<LtAllocationMapDocument, AssetError> {
    let bytes = fs::read(input)?;
    let elf = Elf32View::parse(&bytes)?;
    let glyph_count = read_lt_runtime_glyph_limit(&elf, &bytes)?;
    let (allocation_size, sector_count) = read_lt_allocation(&elf, &bytes)?;
    let glyph_bytes = glyph_count.checked_mul(LT_GLYPH_BYTES).ok_or_else(|| {
        AssetError::InvalidProject("EBOOT LT runtime glyph byte span overflows u32".to_owned())
    })?;
    if glyph_bytes > allocation_size {
        return Err(AssetError::InvalidProject(format!(
            "EBOOT LT runtime glyph limit {glyph_count:#x} requires at least {glyph_bytes:#x} glyph bytes, but allocation is only {allocation_size:#x}; build lt.bin with rz-tool before extracting its allocation map"
        )));
    }
    Ok(LtAllocationMapDocument {
        format: "rz-lt-allocation-map".to_owned(),
        version: 1,
        source_eboot_sha256: sha256_hex(&bytes),
        glyph_count,
        glyph_count_hex: format!("{glyph_count:#x}"),
        allocation_size: usize::try_from(allocation_size).map_err(|_| {
            AssetError::InvalidProject("LT allocation size exceeds usize".to_owned())
        })?,
        allocation_size_hex: format!("{allocation_size:#x}"),
        sector_count,
        stock_glyph_count: LT_STOCK_GLYPH_COUNT,
        stock_allocation_size: LT_STOCK_ALLOCATION_SIZE as usize,
        stock_sector_count: LT_STOCK_SECTOR_COUNT,
        patched: glyph_count != LT_STOCK_GLYPH_COUNT
            || allocation_size != LT_STOCK_ALLOCATION_SIZE
            || sector_count != LT_STOCK_SECTOR_COUNT,
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

pub fn patch_sc_elf(
    input: &Path,
    output: &Path,
    plan: &ScPatchPlan,
    require_vwf_runtime: bool,
    force: bool,
) -> Result<EbootPatchReport, AssetError> {
    let mut bytes = fs::read(input)?;
    let elf = Elf32View::parse(&bytes)?;
    let sector_offset = elf.virtual_to_file_offset(SC_SECTOR_TABLE_VA, SC_ENTRY_COUNT * 4)?;
    let metadata_offset = elf.virtual_to_file_offset(SC_METADATA_TABLE_VA, SC_ENTRY_COUNT * 12)?;
    let buffer_offset = elf.virtual_to_file_offset(SC_BUFFER_MOV_VA, 4)?;

    validate_existing_sector_table(&bytes[sector_offset..sector_offset + SC_ENTRY_COUNT * 4])?;
    validate_existing_metadata_table(&bytes[metadata_offset..metadata_offset + SC_ENTRY_COUNT * 12])?;
    let current_buffer = decode_buffer_mov(&bytes[buffer_offset..buffer_offset + 4]).ok_or_else(|| {
        AssetError::InvalidProject(format!(
            "eboot instruction at {SC_BUFFER_MOV_VA:#010x} is not a supported `movs.w r0, #power_of_two` signature"
        ))
    })?;
    let forced_runtime_hash_mismatch = if require_vwf_runtime {
        validate_vwf_runtime_patch(&elf, &bytes, force)?
    } else {
        false
    };

    let mut start_sector = 0u32;
    for (index, entry) in plan.entries.iter().enumerate() {
        if start_sector > u32::from(u16::MAX) {
            return Err(AssetError::InvalidProject(
                "rebuilt SC start-sector table exceeds u16".to_owned(),
            ));
        }
        let base = sector_offset + index * 4;
        bytes[base..base + 2].copy_from_slice(&(start_sector as u16).to_le_bytes());
        bytes[base + 2..base + 4].copy_from_slice(&entry.sectors.to_le_bytes());
        start_sector = start_sector
            .checked_add(u32::from(entry.sectors))
            .ok_or_else(|| AssetError::InvalidProject("SC sector total overflows".to_owned()))?;
    }
    if start_sector > u32::from(u16::MAX) {
        return Err(AssetError::InvalidProject(format!(
            "rebuilt SC archive requires {start_sector} sectors, exceeding the engine table's u16 address space"
        )));
    }

    let mut stream_base = 0u32;
    let mut primary_base = 0u32;
    let mut secondary_base = 0u32;
    for (index, entry) in plan.entries.iter().enumerate() {
        for (name, value) in [
            ("stream global base", stream_base),
            ("primary global base", primary_base),
            ("secondary global base", secondary_base),
        ] {
            if value > u32::from(u16::MAX) {
                return Err(AssetError::InvalidProject(format!(
                    "SC {name} exceeds u16 at entry {index}"
                )));
            }
        }
        let base = metadata_offset + index * 12;
        write_u16(&mut bytes, base, stream_base as u16);
        write_u16(&mut bytes, base + 2, entry.stream_count);
        write_u16(&mut bytes, base + 4, primary_base as u16);
        write_u16(&mut bytes, base + 6, entry.primary_count);
        write_u16(&mut bytes, base + 8, secondary_base as u16);
        write_u16(&mut bytes, base + 10, entry.secondary_count);

        stream_base = stream_base
            .checked_add(u32::from(entry.stream_count))
            .ok_or_else(|| AssetError::InvalidProject("SC stream base overflows".to_owned()))?;
        primary_base = primary_base
            .checked_add((u32::from(entry.primary_count) + 7) / 8)
            .ok_or_else(|| AssetError::InvalidProject("SC primary base overflows".to_owned()))?;
        secondary_base = secondary_base
            .checked_add(u32::from(entry.secondary_count))
            .ok_or_else(|| AssetError::InvalidProject("SC secondary base overflows".to_owned()))?;
    }

    let required_buffer = next_power_of_two_at_least(plan.max_allocation(), 0x20000)?;
    let required_buffer_u32 = u32::try_from(required_buffer).map_err(|_| {
        AssetError::InvalidProject("SC runtime buffer size exceeds u32".to_owned())
    })?;
    let encoded = encode_buffer_mov(required_buffer_u32).ok_or_else(|| {
        AssetError::InvalidProject(format!(
            "SC runtime buffer {required_buffer_u32:#x} is not encodable by the four-byte Thumb immediate at {SC_BUFFER_MOV_VA:#010x}"
        ))
    })?;
    bytes[buffer_offset..buffer_offset + 4].copy_from_slice(&encoded);

    fs::write(output, &bytes)?;
    Ok(EbootPatchReport {
        changed: plan.differs_from_stock() || current_buffer != required_buffer_u32,
        script_buffer_size: required_buffer_u32,
        total_sectors: start_sector as u16,
        forced_runtime_hash_mismatch,
    })
}

pub fn patch_lt_elf(
    input: &Path,
    output: &Path,
    plan: LtPatchPlan,
    force: bool,
) -> Result<LtEbootPatchReport, AssetError> {
    let mut bytes = fs::read(input)?;
    let elf = Elf32View::parse(&bytes)?;
    let forced_runtime_hash_mismatch = validate_vwf_runtime_patch(&elf, &bytes, force)?;
    let runtime_glyph_limit = read_lt_runtime_glyph_limit(&elf, &bytes)?;
    if runtime_glyph_limit != plan.glyph_count {
        return Err(AssetError::InvalidProject(format!(
            "lt.bin project glyph_count {:#x} does not match EBOOT runtime glyph limit {runtime_glyph_limit:#x}; rebuild the VWF EBOOT with the matching font.tbl before building lt.bin",
            plan.glyph_count
        )));
    }
    if plan.allocation_size == 0 || plan.allocation_size % SECTOR_SIZE != 0 {
        return Err(AssetError::InvalidProject(format!(
            "rebuilt lt.bin allocation size {:#x} is not a non-zero {SECTOR_SIZE:#x}-byte sector multiple",
            plan.allocation_size
        )));
    }
    let allocation_size = u32::try_from(plan.allocation_size).map_err(|_| {
        AssetError::InvalidProject("rebuilt lt.bin allocation size exceeds u32".to_owned())
    })?;
    let glyph_bytes = plan.glyph_count.checked_mul(LT_GLYPH_BYTES).ok_or_else(|| {
        AssetError::InvalidProject("rebuilt lt.bin runtime glyph byte span overflows u32".to_owned())
    })?;
    if glyph_bytes > allocation_size {
        return Err(AssetError::InvalidProject(format!(
            "rebuilt lt.bin allocation {allocation_size:#x} is smaller than runtime glyph span {glyph_bytes:#x} for glyph_count {:#x}",
            plan.glyph_count
        )));
    }
    let sector_count = u32::try_from(plan.allocation_size / SECTOR_SIZE).map_err(|_| {
        AssetError::InvalidProject("rebuilt lt.bin sector count exceeds u32".to_owned())
    })?;
    let (current_size, current_sectors) = read_lt_allocation(&elf, &bytes)?;
    let changed = current_size != allocation_size || current_sectors != sector_count;

    if changed {
        let load_offset = elf.virtual_to_file_offset(LT_LOAD_SIZE_VA, 8)?;
        let low = u16::try_from(allocation_size & 0xffff).unwrap();
        let high = u16::try_from(allocation_size >> 16).unwrap();
        bytes[load_offset..load_offset + 4].copy_from_slice(&encode_thumb_mov_imm16(0xf240, 0, low));
        bytes[load_offset + 4..load_offset + 8].copy_from_slice(&encode_thumb_mov_imm16(0xf2c0, 0, high));

        let size_offset = elf.virtual_to_file_offset(LT_SIZE_TABLE_VA, 4)?;
        let count_offset = elf.virtual_to_file_offset(LT_COUNT_TABLE_VA, 4)?;
        bytes[size_offset..size_offset + 4].copy_from_slice(&allocation_size.to_le_bytes());
        bytes[count_offset..count_offset + 4].copy_from_slice(&sector_count.to_le_bytes());
    }

    fs::write(output, &bytes)?;
    Ok(LtEbootPatchReport {
        changed,
        glyph_count: plan.glyph_count,
        allocation_size,
        sector_count,
        forced_runtime_hash_mismatch,
    })
}

fn write_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn validate_vwf_runtime_patch(
    elf: &Elf32View,
    bytes: &[u8],
    force: bool,
) -> Result<bool, AssetError> {
    let range_offset = elf.virtual_to_file_offset(
        VWF_RUNTIME_HASH_RANGE_START_VA,
        VWF_RUNTIME_HASH_RANGE_SIZE,
    )?;
    let range = &bytes[range_offset..range_offset + VWF_RUNTIME_HASH_RANGE_SIZE];
    let actual = sha256_hex(range);
    if actual != VWF_RUNTIME_HASH_RANGE_SHA256 {
        if force {
            return Ok(true);
        }
        return Err(AssetError::InvalidProject(format!(
            "--eboot-in VWF runtime hash mismatch for virtual range {:#010x}..{:#010x}: expected {}, got {}; pass -f/--force to continue with this EBOOT",
            VWF_RUNTIME_HASH_RANGE_START_VA,
            VWF_RUNTIME_HASH_RANGE_END_VA,
            VWF_RUNTIME_HASH_RANGE_SHA256,
            actual
        )));
    }
    Ok(false)
}

fn read_lt_runtime_glyph_limit(elf: &Elf32View, bytes: &[u8]) -> Result<u32, AssetError> {
    let mut decoded = None::<u32>;
    for &(va, register) in LT_RUNTIME_GLYPH_LIMIT_SITES {
        let offset = elf.virtual_to_file_offset(va, 4)?;
        let value = decode_thumb_mov_imm16(&bytes[offset..offset + 4], 0xf240, register)
            .ok_or_else(|| {
                AssetError::InvalidProject(format!(
                    "EBOOT LT runtime glyph-limit instruction at {va:#010x} is not a supported Thumb MOVW r{register}, #imm16"
                ))
            })?;
        let value = u32::from(value);
        if let Some(expected) = decoded {
            if value != expected {
                return Err(AssetError::InvalidProject(format!(
                    "EBOOT LT runtime glyph-limit sites disagree: expected {expected:#x}, found {value:#x} at {va:#010x}"
                )));
            }
        } else {
            decoded = Some(value);
        }
    }
    decoded.ok_or_else(|| AssetError::InvalidProject("EBOOT has no LT runtime glyph-limit sites".to_owned()))
}

fn read_lt_allocation(elf: &Elf32View, bytes: &[u8]) -> Result<(u32, u32), AssetError> {
    let load_offset = elf.virtual_to_file_offset(LT_LOAD_SIZE_VA, 8)?;
    let load_bytes = &bytes[load_offset..load_offset + 8];
    let load_size = if load_bytes == &[0x5f, 0xf4, 0x58, 0x40, 0xc0, 0xf2, 0x0f, 0x00] {
        LT_STOCK_ALLOCATION_SIZE
    } else {
        let low = decode_thumb_mov_imm16(&load_bytes[..4], 0xf240, 0).ok_or_else(|| {
            AssetError::InvalidProject(format!(
                "EBOOT LT allocation instruction at {LT_LOAD_SIZE_VA:#010x} is not the stock MOVS.W form or a supported MOVW r0, #imm16"
            ))
        })?;
        let high = decode_thumb_mov_imm16(&load_bytes[4..], 0xf2c0, 0).ok_or_else(|| {
            AssetError::InvalidProject(format!(
                "EBOOT LT allocation high-half instruction at {:#010x} is not a supported MOVT r0, #imm16",
                LT_LOAD_SIZE_VA + 4
            ))
        })?;
        u32::from(low) | (u32::from(high) << 16)
    };
    let size_offset = elf.virtual_to_file_offset(LT_SIZE_TABLE_VA, 4)?;
    let count_offset = elf.virtual_to_file_offset(LT_COUNT_TABLE_VA, 4)?;
    let table_size = read_u32(bytes, size_offset)?;
    let sector_count = read_u32(bytes, count_offset)?;
    if load_size != table_size {
        return Err(AssetError::InvalidProject(format!(
            "EBOOT LT allocation constants disagree: loader={load_size:#x}, table={table_size:#x}"
        )));
    }
    let sector_size = u32::try_from(SECTOR_SIZE).unwrap();
    let sector_bytes = sector_count.checked_mul(sector_size).ok_or_else(|| {
        AssetError::InvalidProject("EBOOT LT sector allocation overflows u32".to_owned())
    })?;
    if sector_bytes != table_size {
        return Err(AssetError::InvalidProject(format!(
            "EBOOT LT sector count {sector_count:#x} allocates {sector_bytes:#x} bytes, but LT size table is {table_size:#x}"
        )));
    }
    Ok((table_size, sector_count))
}

fn decode_thumb_mov_imm16(bytes: &[u8], base: u16, register: u8) -> Option<u16> {
    if bytes.len() != 4 || register > 15 {
        return None;
    }
    let first = u16::from_le_bytes([bytes[0], bytes[1]]);
    let second = u16::from_le_bytes([bytes[2], bytes[3]]);
    if first & 0xfbf0 != base || ((second >> 8) & 0x0f) != u16::from(register) {
        return None;
    }
    let imm4 = first & 0x000f;
    let i = (first >> 10) & 1;
    let imm3 = (second >> 12) & 0x0007;
    let imm8 = second & 0x00ff;
    Some((imm4 << 12) | (i << 11) | (imm3 << 8) | imm8)
}

fn encode_thumb_mov_imm16(base: u16, register: u8, immediate: u16) -> [u8; 4] {
    let first = base
        | ((immediate >> 12) & 0x000f)
        | (((immediate >> 11) & 1) << 10);
    let second = (((immediate >> 8) & 0x0007) << 12)
        | (u16::from(register) << 8)
        | (immediate & 0x00ff);
    let first = first.to_le_bytes();
    let second = second.to_le_bytes();
    [first[0], first[1], second[0], second[1]]
}

fn validate_existing_sector_table(bytes: &[u8]) -> Result<(), AssetError> {
    let mut expected_start = 0u32;
    for index in 0..SC_ENTRY_COUNT {
        let base = index * 4;
        let start = u16::from_le_bytes([bytes[base], bytes[base + 1]]);
        let count = u16::from_le_bytes([bytes[base + 2], bytes[base + 3]]);
        if u32::from(start) != expected_start || count == 0 {
            return Err(AssetError::InvalidProject(format!(
                "eboot SC sector table is not cumulative at entry {index}: start={start:#x}, count={count:#x}, expected start={expected_start:#x}"
            )));
        }
        expected_start = expected_start
            .checked_add(u32::from(count))
            .ok_or_else(|| AssetError::InvalidProject("eboot SC sector table overflows".to_owned()))?;
    }
    Ok(())
}

fn validate_existing_metadata_table(bytes: &[u8]) -> Result<(), AssetError> {
    let mut expected_stream = 0u32;
    let mut expected_primary = 0u32;
    let mut expected_secondary = 0u32;
    for index in 0..SC_ENTRY_COUNT {
        let base = index * 12;
        let values = (0..6)
            .map(|word| {
                let offset = base + word * 2;
                u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
            })
            .collect::<Vec<_>>();
        if u32::from(values[0]) != expected_stream
            || u32::from(values[2]) != expected_primary
            || u32::from(values[4]) != expected_secondary
        {
            return Err(AssetError::InvalidProject(format!(
                "eboot SC metadata bases are inconsistent at entry {index}"
            )));
        }
        expected_stream += u32::from(values[1]);
        expected_primary += (u32::from(values[3]) + 7) / 8;
        expected_secondary += u32::from(values[5]);
    }
    Ok(())
}

fn next_power_of_two_at_least(value: usize, minimum: usize) -> Result<usize, AssetError> {
    value
        .max(minimum)
        .checked_next_power_of_two()
        .ok_or_else(|| AssetError::InvalidProject("SC runtime buffer power-of-two rounding overflows".to_owned()))
}

fn decode_buffer_mov(bytes: &[u8]) -> Option<u32> {
    const VALUES: &[(u32, [u8; 4])] = &[
        (0x0002_0000, [0x5f, 0xf4, 0x00, 0x30]),
        (0x0004_0000, [0x5f, 0xf4, 0x80, 0x20]),
        (0x0008_0000, [0x5f, 0xf4, 0x00, 0x20]),
        (0x0010_0000, [0x5f, 0xf4, 0x80, 0x10]),
        (0x0020_0000, [0x5f, 0xf4, 0x00, 0x10]),
        (0x0040_0000, [0x5f, 0xf4, 0x80, 0x00]),
        (0x0080_0000, [0x5f, 0xf4, 0x00, 0x00]),
        (0x0100_0000, [0x5f, 0xf0, 0x80, 0x70]),
        (0x0200_0000, [0x5f, 0xf0, 0x00, 0x70]),
        (0x0400_0000, [0x5f, 0xf0, 0x80, 0x60]),
        (0x0800_0000, [0x5f, 0xf0, 0x00, 0x60]),
    ];
    VALUES
        .iter()
        .find_map(|(value, encoding)| (bytes == encoding).then_some(*value))
}

fn encode_buffer_mov(value: u32) -> Option<[u8; 4]> {
    const VALUES: &[(u32, [u8; 4])] = &[
        (0x0002_0000, [0x5f, 0xf4, 0x00, 0x30]),
        (0x0004_0000, [0x5f, 0xf4, 0x80, 0x20]),
        (0x0008_0000, [0x5f, 0xf4, 0x00, 0x20]),
        (0x0010_0000, [0x5f, 0xf4, 0x80, 0x10]),
        (0x0020_0000, [0x5f, 0xf4, 0x00, 0x10]),
        (0x0040_0000, [0x5f, 0xf4, 0x80, 0x00]),
        (0x0080_0000, [0x5f, 0xf4, 0x00, 0x00]),
        (0x0100_0000, [0x5f, 0xf0, 0x80, 0x70]),
        (0x0200_0000, [0x5f, 0xf0, 0x00, 0x70]),
        (0x0400_0000, [0x5f, 0xf0, 0x80, 0x60]),
        (0x0800_0000, [0x5f, 0xf0, 0x00, 0x60]),
    ];
    VALUES
        .iter()
        .find_map(|(candidate, encoding)| (*candidate == value).then_some(*encoding))
}

struct Elf32View {
    program_headers: Vec<ProgramHeader>,
}

#[derive(Debug, Clone, Copy)]
struct ProgramHeader {
    kind: u32,
    offset: u32,
    virtual_address: u32,
    file_size: u32,
}

impl Elf32View {
    fn parse(bytes: &[u8]) -> Result<Self, AssetError> {
        if bytes.len() < 52 || &bytes[..4] != b"\x7fELF" || bytes[4] != 1 || bytes[5] != 1 {
            return Err(AssetError::InvalidProject(
                "--eboot-in must be a 32-bit little-endian ELF image (for example eboot.bin.elf)"
                    .to_owned(),
            ));
        }
        let phoff = read_u32(bytes, 28)? as usize;
        let phentsize = usize::from(read_u16(bytes, 42)?);
        let phnum = usize::from(read_u16(bytes, 44)?);
        if phentsize < 32 {
            return Err(AssetError::InvalidProject(
                "eboot ELF program-header size is smaller than ELF32".to_owned(),
            ));
        }
        let mut headers = Vec::with_capacity(phnum);
        for index in 0..phnum {
            let base = phoff
                .checked_add(index.checked_mul(phentsize).ok_or_else(|| {
                    AssetError::InvalidProject("eboot ELF program-header offset overflows".to_owned())
                })?)
                .ok_or_else(|| AssetError::InvalidProject("eboot ELF program-header offset overflows".to_owned()))?;
            if base + 32 > bytes.len() {
                return Err(AssetError::InvalidProject(
                    "eboot ELF program-header table is truncated".to_owned(),
                ));
            }
            headers.push(ProgramHeader {
                kind: read_u32(bytes, base)?,
                offset: read_u32(bytes, base + 4)?,
                virtual_address: read_u32(bytes, base + 8)?,
                file_size: read_u32(bytes, base + 16)?,
            });
        }
        Ok(Self {
            program_headers: headers,
        })
    }

    fn virtual_to_file_offset(&self, address: u32, size: usize) -> Result<usize, AssetError> {
        for header in &self.program_headers {
            if header.kind != 1 {
                continue;
            }
            let end = address.checked_add(u32::try_from(size).map_err(|_| {
                AssetError::InvalidProject("eboot patch size exceeds u32".to_owned())
            })?).ok_or_else(|| AssetError::InvalidProject("eboot patch address overflows".to_owned()))?;
            let segment_end = header.virtual_address.checked_add(header.file_size).ok_or_else(|| {
                AssetError::InvalidProject("eboot ELF segment address overflows".to_owned())
            })?;
            if address >= header.virtual_address && end <= segment_end {
                let file_offset = header
                    .offset
                    .checked_add(address - header.virtual_address)
                    .ok_or_else(|| {
                        AssetError::InvalidProject("eboot file offset overflows u32".to_owned())
                    })?;
                return usize::try_from(file_offset).map_err(|_| {
                    AssetError::InvalidProject("eboot file offset exceeds usize".to_owned())
                });
            }
        }
        Err(AssetError::InvalidProject(format!(
            "eboot ELF does not map virtual range {address:#010x}..{:#010x}",
            address.saturating_add(u32::try_from(size).unwrap_or(u32::MAX))
        )))
    }
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, AssetError> {
    let value = bytes.get(offset..offset + 2).ok_or_else(|| {
        AssetError::InvalidProject("eboot ELF header is truncated".to_owned())
    })?;
    Ok(u16::from_le_bytes([value[0], value[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, AssetError> {
    let value = bytes.get(offset..offset + 4).ok_or_else(|| {
        AssetError::InvalidProject("eboot ELF header is truncated".to_owned())
    })?;
    Ok(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_buffer_immediates_round_trip() {
        for value in [
            0x0002_0000, 0x0004_0000, 0x0008_0000, 0x0010_0000,
            0x0020_0000, 0x0040_0000, 0x0080_0000, 0x0100_0000,
            0x0200_0000, 0x0400_0000, 0x0800_0000,
        ] {
            let encoded = encode_buffer_mov(value).expect("supported immediate");
            assert_eq!(decode_buffer_mov(&encoded), Some(value));
        }
        assert_eq!(encode_buffer_mov(0x0003_0000), None);
    }

    #[test]
    fn buffer_rounding_has_stock_floor() {
        assert_eq!(next_power_of_two_at_least(1, 0x20000).unwrap(), 0x20000);
        assert_eq!(next_power_of_two_at_least(0x20001, 0x20000).unwrap(), 0x40000);
    }

    #[test]
    fn thumb_movw_movt_immediates_round_trip() {
        for &(base, register, immediate) in &[
            (0xf240, 0, 0x0e12),
            (0xf240, 1, 0x0f40),
            (0xf240, 0, 0x3000),
            (0xf2c0, 0, 0x0011),
        ] {
            let encoded = encode_thumb_mov_imm16(base, register, immediate);
            assert_eq!(decode_thumb_mov_imm16(&encoded, base, register), Some(immediate));
        }
    }

    #[test]
    fn vwf_runtime_range_hash_constant_matches_patcher_output() {
        assert_eq!(VWF_RUNTIME_HASH_RANGE_START_VA, 0x8100_0000);
        assert_eq!(VWF_RUNTIME_HASH_RANGE_END_VA, 0x8110_0000);
        assert_eq!(VWF_RUNTIME_HASH_RANGE_SIZE, 0x0010_0000);
        assert_eq!(
            VWF_RUNTIME_HASH_RANGE_SHA256,
            "c7cc66521264acdc6d259ad189d1a6fe731901855a6708ae3a3b036334091a84"
        );
    }
}
