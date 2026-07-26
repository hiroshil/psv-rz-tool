use std::fs;
use std::path::Path;

use crate::engine_allocations::{SC_METADATA, SC_SECTORS, SECTOR_SIZE};
use crate::error::AssetError;

pub const SC_ENTRY_COUNT: usize = 89;
const SC_SECTOR_TABLE_VA: u32 = 0x8111_344c;
const SC_METADATA_TABLE_VA: u32 = 0x810f_9b1c;
const SC_BUFFER_MOV_VA: u32 = 0x8101_b554;

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

pub fn patch_sc_elf(
    input: &Path,
    output: &Path,
    plan: &ScPatchPlan,
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
    })
}

fn write_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
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
}
