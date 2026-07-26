use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::codec::charset;
use crate::engine_allocations::{self, ScMetadata};
use crate::error::AssetError;
use crate::manifest::{AssetEntry, AssetKind};

const DOCUMENT_VERSION: u32 = 1;
const SOURCE_DOCUMENT_VERSION: u32 = 1;
const ROUTING_DOCUMENT_VERSION: u32 = 1;
const VOICE_HEADER_SIZE: usize = 0x80;
const RUNTIME_BASE: usize = 0x80;
const SCRIPT_BASE: usize = 0x2000;
const RUNTIME_HEADER_SIZE: usize = 0x10;
const SECONDARY_RECORD_SIZE: usize = 0x1c;
const OPAQUE_FOOTER_SIZE: usize = 0x10;
const VOICE_NOT_EXIST: &[u8] = b"voice_not_exist\0";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptDocument {
    pub document_version: u32,
    pub entry_id: u32,
    /// Original CPK payload size. An analyzed SC loader reads the fixed sector
    /// allocation, so build output is padded to allocation_size.
    pub source_size: u32,
    pub allocation_size: u32,
    /// Bytes available for the logical script payload before the fixed 16-byte
    /// opaque footer. Zero fill between logical payload and footer is capacity,
    /// not source IR.
    pub payload_capacity_bytes: u32,
    /// Last 16 bytes of the allocated SC payload. No located VM consumer reads
    /// them, but their producer/checksum semantics are unproven, so they remain
    /// explicit and are placed at the end of the rebuilt allocation.
    pub opaque_footer_hex: String,
    /// Rebuild-authoritative representation of entry+0x0000..0x007f.
    pub voice_header: ScriptVoiceHeader,
    /// User-editable, rebuild-authoritative source-equivalent IR. It contains
    /// typed Unicode dialogue plus exact raw u16 nodes for commands/data whose
    /// source syntax is not proven.
    pub editable: String,
    pub trailing_byte: Option<u8>,
    /// Rebuild-authoritative but machine-managed relocation records. Payload
    /// addresses are labels; small non-address values remain immediate u32s.
    /// This field belongs to the non-editable metadata document so normal text
    /// editing cannot accidentally corrupt the engine's auxiliary indexes.
    pub secondary_records: Vec<ScriptSourceSecondaryRecord>,
    /// Executable-resident global-base/count record for this SC entry. This is
    /// an annotation and a build invariant, not embedded script payload.
    pub engine_metadata: Option<ScriptEngineMetadataAnnotation>,
    /// Derived view of the dense stream-offset table at entry+0x2000.
    pub stream_table: ScriptOffsetTableAnnotation,
    /// Derived primary marker table. Build regenerates this table by locating
    /// the ordered FFF0 0000, FFF0 0001, ... marker chain in the payload.
    pub primary_markers: ScriptPrimaryMarkerAnnotation,
    pub resource_routing: ScriptResourceRoutingAnnotation,
    pub limitations: Vec<String>,
}


#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptSourceDocument {
    pub document_version: u32,
    pub entry_id: u32,
    pub charset: String,
    pub stream_count: u32,
    pub nodes: Vec<ScriptSourceNode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secondary_records: Vec<ScriptSourceSecondaryRecord>,
    pub relocation_model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioRoutingDocument {
    pub document_version: u32,
    pub archive: String,
    pub identity_model: String,
    pub chronology_model: String,
    pub transition_model: String,
    pub startup: ScenarioRouteTarget,
    pub entries: Vec<ScenarioEntryIdentity>,
    pub transitions: Vec<ScenarioTransition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioRouteTarget {
    pub entry_id: u32,
    pub stream_id: u32,
    pub evidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioEntryIdentity {
    pub entry_id: u32,
    pub archive_order: u32,
    pub stream_count: u32,
    pub editable: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioTransition {
    pub source_entry_id: u32,
    pub source_node_index: u32,
    pub source_word_index: u32,
    pub target_entry_id: u32,
    pub target_stream_id: u32,
    pub opcode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ScriptSourceNode {
    Raw {
        #[serde(default)]
        labels: Vec<String>,
        words: Vec<u16>,
    },
    Dialogue {
        #[serde(default)]
        labels: Vec<String>,
        marker_index: u32,
        speaker: String,
        /// Original glyph IDs are machine-managed round-trip state. They are
        /// reused only while they still decode to `speaker`, preserving charset
        /// aliases without preventing Unicode edits.
        speaker_source_glyphs: Vec<u16>,
        /// Ordered screen/page bodies. Each page is terminated by FFFE in the
        /// compiled stream; preserving the boundary is required because the
        /// interpreter waits/advances at every terminator.
        pages: Vec<ScriptDialoguePage>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptDialoguePage {
    pub text: String,
    /// Original glyph IDs for byte-exact no-edit rebuild. If `text` changes,
    /// the assembler ignores this field and encodes the edited Unicode string.
    pub source_glyphs: Vec<u16>,
}

impl ScriptSourceNode {
    fn labels(&self) -> &[String] {
        match self {
            Self::Raw { labels, .. } | Self::Dialogue { labels, .. } => labels,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptSourceSecondaryRecord {
    pub fields: [ScriptSourceValue; 7],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ScriptSourceValue {
    Immediate { value: u32 },
    Label { name: String },
}

#[derive(Debug, Clone, Copy)]
struct ScriptPageRange {
    start: usize,
    end: usize,
}

#[derive(Debug, Clone)]
struct ScriptTextRange {
    marker_index: u32,
    byte_offset: u32,
    speaker_start: usize,
    speaker_end: usize,
    pages: Vec<ScriptPageRange>,
    end: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ScriptVoiceHeader {
    VoiceBase { base: u16 },
    VoiceNotExist,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScriptSecondaryRecord {
    pub words: [u32; 7],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptEngineMetadataAnnotation {
    pub address: String,
    pub stream_global_base: u16,
    pub stream_count: u16,
    pub primary_global_base: u16,
    pub primary_count: u16,
    pub secondary_global_base: u16,
    pub secondary_count: u16,
    pub meaning: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptOffsetTableAnnotation {
    pub table_bytes: u32,
    pub entries: Vec<ScriptOffsetAnnotation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptOffsetAnnotation {
    pub index: u32,
    pub offset: u32,
    pub even_aligned: bool,
    pub inside_payload: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptPrimaryMarkerAnnotation {
    pub marker_word: String,
    pub derived_count: u32,
    pub offsets: Vec<u32>,
    pub generation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptResourceRoutingAnnotation {
    pub embedded_image_bytes: bool,
    pub archives: Vec<ScriptResourceArchiveAnnotation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptResourceArchiveAnnotation {
    pub archive: String,
    pub role: String,
    pub evidence: Vec<String>,
}

struct ParsedScript {
    source_size: u32,
    allocation_size: u32,
    payload_capacity_bytes: u32,
    opaque_footer: Vec<u8>,
    voice_header: ScriptVoiceHeader,
    words: Vec<u16>,
    trailing_byte: Option<u8>,
    secondary_records: Vec<ScriptSecondaryRecord>,
    metadata: Option<ScMetadata>,
    stream_table: ScriptOffsetTableAnnotation,
    primary_offsets: Vec<u32>,
}

#[derive(Debug, Clone, Copy)]
pub struct ScriptBuildInfo {
    pub required_allocation: usize,
    pub stream_count: u16,
    pub primary_count: u16,
    pub secondary_count: u16,
}

pub fn decode(
    input: &[u8],
    asset_directory: &Path,
    output_stem: &str,
    entry_id: u32,
    allocation_size: usize,
) -> Result<AssetKind, AssetError> {
    let parsed = parse(input, entry_id, allocation_size)?;
    let editable_name = format!("{output_stem}.script.json");
    let document_name = format!("{output_stem}.script-meta.json");

    let mut source_document = build_source_document(
        entry_id,
        &parsed.words,
        &parsed.stream_table,
        &parsed.primary_offsets,
        &parsed.secondary_records,
    )?;
    let secondary_records = std::mem::take(&mut source_document.secondary_records);
    fs::write(
        asset_directory.join(&editable_name),
        serde_json::to_vec_pretty(&source_document)?,
    )?;

    let document = ScriptDocument {
        document_version: DOCUMENT_VERSION,
        entry_id,
        source_size: parsed.source_size,
        allocation_size: parsed.allocation_size,
        payload_capacity_bytes: parsed.payload_capacity_bytes,
        opaque_footer_hex: encode_hex(&parsed.opaque_footer),
        voice_header: parsed.voice_header,
        editable: editable_name,
        trailing_byte: parsed.trailing_byte,
        secondary_records,
        engine_metadata: parsed.metadata.map(metadata_annotation),
        stream_table: parsed.stream_table,
        primary_markers: primary_annotation(parsed.primary_offsets),
        resource_routing: resource_routing(),
        limitations: limitations(),
    };
    fs::write(
        asset_directory.join(&document_name),
        serde_json::to_vec_pretty(&document)?,
    )?;
    Ok(AssetKind::Script {
        document: document_name,
    })
}

pub fn write_routing_document(
    asset_directory: &Path,
    assets: &[AssetEntry],
) -> Result<(), AssetError> {
    let mut documents = BTreeMap::<u32, (u32, String, ScriptSourceDocument)>::new();
    for asset in assets {
        let AssetKind::Script { document } = &asset.kind else {
            return Err(AssetError::InvalidProject(format!(
                "sc.cpk entry {} is not a script document",
                asset.file_name
            )));
        };
        let entry_id = asset.id.ok_or_else(|| {
            AssetError::InvalidProject(format!(
                "sc.cpk entry {} has no ITOC engine ID",
                asset.file_name
            ))
        })?;
        let metadata: ScriptDocument =
            serde_json::from_slice(&fs::read(asset_directory.join(document))?)?;
        let source: ScriptSourceDocument =
            serde_json::from_slice(&fs::read(asset_directory.join(&metadata.editable))?)?;
        if metadata.entry_id != entry_id || source.entry_id != entry_id {
            return Err(AssetError::InvalidProject(format!(
                "sc.cpk manifest/document entry ID mismatch for {entry_id}"
            )));
        }
        if documents
            .insert(entry_id, (asset.order, metadata.editable.clone(), source))
            .is_some()
        {
            return Err(AssetError::InvalidProject(format!(
                "sc.cpk has duplicate engine ID {entry_id}"
            )));
        }
    }

    let mut transitions = Vec::new();
    for (&source_entry_id, (_, _, source)) in &documents {
        for (node_index, node) in source.nodes.iter().enumerate() {
            let ScriptSourceNode::Raw { words, .. } = node else {
                continue;
            };
            for word_index in 0..words.len().saturating_sub(2) {
                if words[word_index] != 0xffef {
                    continue;
                }
                let target_entry_id = u32::from(words[word_index + 1]);
                let target_stream_id = u32::from(words[word_index + 2]);
                let Some((_, _, target)) = documents.get(&target_entry_id) else {
                    continue;
                };
                if target_stream_id >= target.stream_count {
                    continue;
                }
                transitions.push(ScenarioTransition {
                    source_entry_id,
                    source_node_index: u32::try_from(node_index).unwrap_or(u32::MAX),
                    source_word_index: u32::try_from(word_index).unwrap_or(u32::MAX),
                    target_entry_id,
                    target_stream_id,
                    opcode: "FFEF".to_owned(),
                });
            }
        }
    }
    transitions.sort_by_key(|transition| {
        (
            transition.source_entry_id,
            transition.source_node_index,
            transition.source_word_index,
        )
    });

    let entries = documents
        .into_iter()
        .map(|(entry_id, (archive_order, editable, source))| ScenarioEntryIdentity {
            entry_id,
            archive_order,
            stream_count: source.stream_count,
            editable,
        })
        .collect();
    let document = ScenarioRoutingDocument {
        document_version: ROUTING_DOCUMENT_VERSION,
        archive: "sc.cpk".to_owned(),
        identity_model: "entry_id is the engine-visible ITOC file ID passed unchanged to FUN_81053CB6; archive_order records CPK iteration/emission order and is not a scenario number"
            .to_owned(),
        chronology_model: "the scenario is a branching directed graph; there is no single total file order. Follow the proven startup and inspect FFEF route candidates"
            .to_owned(),
        transition_model: "each item is an FFEF word triple found in a raw node whose target entry and stream are in range. The complete opcode-width/control-flow grammar is not recovered, so this is navigation evidence rather than a guaranteed execution trace"
            .to_owned(),
        startup: ScenarioRouteTarget {
            entry_id: 0x56,
            stream_id: 0,
            evidence: "new-game initialization at 0x8101AD94 calls FUN_8101BEC6 with r0=0x56 and r3=0"
                .to_owned(),
        },
        entries,
        transitions,
    };
    fs::write(
        asset_directory.join("scenario-routing.json"),
        serde_json::to_vec_pretty(&document)?,
    )?;
    Ok(())
}

pub fn encode(path: &Path) -> Result<Vec<u8>, AssetError> {
    let document: ScriptDocument = serde_json::from_slice(&fs::read(path)?)?;
    let allocation_size = usize::try_from(document.allocation_size)
        .map_err(|_| AssetError::InvalidProject("script allocation exceeds usize".to_owned()))?;
    encode_with_allocation(path, allocation_size, false)
}

pub fn inspect_build(path: &Path) -> Result<ScriptBuildInfo, AssetError> {
    inspect_build_with_charset(path, charset::default_map())
}

pub fn inspect_build_with_charset(
    path: &Path,
    charset_map: &charset::CharsetMap,
) -> Result<ScriptBuildInfo, AssetError> {
    let document: ScriptDocument = serde_json::from_slice(&fs::read(path)?)?;
    validate_document_version(&document)?;
    let root = path.parent().unwrap_or_else(|| Path::new("."));
    let mut source_document: ScriptSourceDocument =
        serde_json::from_slice(&fs::read(root.join(&document.editable))?)?;
    source_document.secondary_records = document.secondary_records.clone();
    let (words, secondary) = assemble_source_document(document.entry_id, &source_document, charset_map)?;
    let stream_table = parse_offset_table(&words_to_bytes(&words)).map_err(|error| {
        AssetError::InvalidProject(format!("relocated script payload is invalid: {error}"))
    })?;
    let primary = derive_primary_offsets(&words)?;
    let mut runtime_probe = vec![0u8; SCRIPT_BASE - RUNTIME_BASE];
    write_runtime_block(&mut runtime_probe, &primary, &secondary).map_err(|error| {
        AssetError::InvalidProject(format!(
            "generated script runtime tables exceed the fixed 0x2000-byte header: {error}"
        ))
    })?;
    let logical_size = SCRIPT_BASE
        .checked_add(words.len().checked_mul(2).ok_or_else(|| {
            AssetError::InvalidProject("script logical size overflows".to_owned())
        })?)
        .and_then(|value| value.checked_add(if document.trailing_byte.is_some() { 1 } else { 0 }))
        .and_then(|value| value.checked_add(OPAQUE_FOOTER_SIZE))
        .ok_or_else(|| AssetError::InvalidProject("script allocation requirement overflows".to_owned()))?;
    let required_allocation = round_up_sector(logical_size)?;
    Ok(ScriptBuildInfo {
        required_allocation,
        stream_count: u16::try_from(stream_table.entries.len()).map_err(|_| {
            AssetError::InvalidProject("script stream count exceeds u16".to_owned())
        })?,
        primary_count: u16::try_from(primary.len()).map_err(|_| {
            AssetError::InvalidProject("script primary count exceeds u16".to_owned())
        })?,
        secondary_count: u16::try_from(secondary.len()).map_err(|_| {
            AssetError::InvalidProject("script secondary count exceeds u16".to_owned())
        })?,
    })
}

pub fn encode_with_allocation(
    path: &Path,
    allocation_size: usize,
    allow_engine_metadata_change: bool,
) -> Result<Vec<u8>, AssetError> {
    encode_with_allocation_and_charset(
        path,
        allocation_size,
        allow_engine_metadata_change,
        charset::default_map(),
    )
}

pub fn encode_with_allocation_and_charset(
    path: &Path,
    allocation_size: usize,
    allow_engine_metadata_change: bool,
    charset_map: &charset::CharsetMap,
) -> Result<Vec<u8>, AssetError> {
    let document: ScriptDocument = serde_json::from_slice(&fs::read(path)?)?;
    validate_document_version(&document)?;
    if allocation_size <= SCRIPT_BASE {
        return Err(AssetError::InvalidProject(
            "script allocation is not larger than the 0x2000-byte generated header area"
                .to_owned(),
        ));
    }
    if usize::try_from(document.source_size).unwrap_or(usize::MAX) > allocation_size {
        return Err(AssetError::InvalidProject(
            "script source_size exceeds allocation_size".to_owned(),
        ));
    }
    let footer = decode_hex(&document.opaque_footer_hex)?;
    if footer.len() != OPAQUE_FOOTER_SIZE {
        return Err(AssetError::InvalidProject(format!(
            "script opaque footer has {} bytes, expected {OPAQUE_FOOTER_SIZE}",
            footer.len()
        )));
    }
    let original_capacity = usize::try_from(document.payload_capacity_bytes).map_err(|_| {
        AssetError::InvalidProject("script payload capacity exceeds usize".to_owned())
    })?;
    let documented_capacity = usize::try_from(document.allocation_size)
        .ok()
        .and_then(|value| value.checked_sub(SCRIPT_BASE + OPAQUE_FOOTER_SIZE))
        .ok_or_else(|| AssetError::InvalidProject("documented script allocation is too small".to_owned()))?;
    if original_capacity != documented_capacity {
        return Err(AssetError::InvalidProject(format!(
            "script payload capacity is {original_capacity:#x}, expected {documented_capacity:#x} from documented allocation"
        )));
    }
    if allocation_size < usize::try_from(document.allocation_size).unwrap_or(usize::MAX) {
        return Err(AssetError::InvalidProject(
            "script allocation override may expand but may not shrink the extracted allocation".to_owned(),
        ));
    }

    let root = path.parent().unwrap_or_else(|| Path::new("."));
    let mut source_document: ScriptSourceDocument =
        serde_json::from_slice(&fs::read(root.join(&document.editable))?)?;
    source_document.secondary_records = document.secondary_records.clone();
    let (words, relocated_secondary) =
        assemble_source_document(document.entry_id, &source_document, charset_map)?;
    let metadata = engine_allocations::sc_metadata(document.entry_id);
    if !allow_engine_metadata_change {
        validate_metadata_annotation(document.engine_metadata.as_ref(), metadata)?;
    }

    let stream_table = parse_offset_table(&words_to_bytes(&words)).map_err(|error| {
        AssetError::InvalidProject(format!("relocated script payload is invalid: {error}"))
    })?;
    if !allow_engine_metadata_change {
        validate_stream_count(&stream_table, metadata, true)?;
    }
    let primary_offsets = derive_primary_offsets(&words)?;
    if !allow_engine_metadata_change {
        validate_primary_count(&primary_offsets, metadata, true)?;
        validate_secondary_count(&relocated_secondary, metadata, true)?;
    }

    let mut output = vec![0u8; SCRIPT_BASE];
    write_voice_header(&mut output[..VOICE_HEADER_SIZE], &document.voice_header)?;
    write_runtime_block(
        &mut output[RUNTIME_BASE..SCRIPT_BASE],
        &primary_offsets,
        &relocated_secondary,
    )?;
    for value in &words {
        output.extend_from_slice(&value.to_le_bytes());
    }
    if let Some(byte) = document.trailing_byte {
        output.push(byte);
    }
    let footer_start = allocation_size - OPAQUE_FOOTER_SIZE;
    if output.len() > footer_start {
        return Err(AssetError::InvalidProject(format!(
            "rebuilt logical script ends at {:#x}, exceeding selected payload capacity {footer_start:#x}",
            output.len()
        )));
    }
    output.resize(footer_start, 0);
    output.extend_from_slice(&footer);

    // Re-parse the exact rebuilt bytes. This verifies that the generated
    // prefix/runtime block, derived primary table, preserved secondary records,
    // stream table, and executable-resident counts agree.
    parse_with_metadata_policy(
        &output,
        document.entry_id,
        allocation_size,
        !allow_engine_metadata_change,
    )
    .map_err(|error| {
        AssetError::InvalidProject(format!("rebuilt script failed engine invariants: {error}"))
    })?;
    Ok(output)
}

fn validate_document_version(document: &ScriptDocument) -> Result<(), AssetError> {
    if document.document_version != DOCUMENT_VERSION {
        return Err(AssetError::InvalidProject(format!(
            "script metadata document version {} is unsupported",
            document.document_version
        )));
    }
    Ok(())
}

fn round_up_sector(value: usize) -> Result<usize, AssetError> {
    value
        .checked_add(engine_allocations::SECTOR_SIZE - 1)
        .map(|value| value / engine_allocations::SECTOR_SIZE * engine_allocations::SECTOR_SIZE)
        .ok_or_else(|| AssetError::InvalidProject("script sector rounding overflows".to_owned()))
}

fn parse(
    input: &[u8],
    entry_id: u32,
    allocation_size: usize,
) -> Result<ParsedScript, AssetError> {
    parse_with_metadata_policy(input, entry_id, allocation_size, true)
}

fn parse_with_metadata_policy(
    input: &[u8],
    entry_id: u32,
    allocation_size: usize,
    validate_engine_metadata: bool,
) -> Result<ParsedScript, AssetError> {
    if input.len() > allocation_size {
        return Err(AssetError::InvalidFormat(format!(
            "script entry has {:#x} bytes, engine allocation is {allocation_size:#x}",
            input.len()
        )));
    }
    if allocation_size <= SCRIPT_BASE {
        return Err(AssetError::InvalidFormat(
            "script allocation is not larger than 0x2000 bytes".to_owned(),
        ));
    }
    let source_size = u32::try_from(input.len())
        .map_err(|_| AssetError::InvalidFormat("script source size exceeds u32".to_owned()))?;
    let mut working = input.to_vec();
    working.resize(allocation_size, 0);

    let voice_header = parse_voice_header(&working[..VOICE_HEADER_SIZE])?;
    let allocated_script = &working[SCRIPT_BASE..];
    if allocated_script.len() <= OPAQUE_FOOTER_SIZE {
        return Err(AssetError::InvalidFormat(
            "script payload is shorter than its opaque 16-byte footer".to_owned(),
        ));
    }
    let footer_start = allocated_script.len() - OPAQUE_FOOTER_SIZE;
    let opaque_footer = allocated_script[footer_start..].to_vec();
    let capacity_region = &allocated_script[..footer_start];
    let trailing_byte = if capacity_region.len() % 2 == 0 {
        None
    } else {
        capacity_region.last().copied()
    };
    let word_end = capacity_region.len() - if trailing_byte.is_some() { 1 } else { 0 };
    let allocated_words = capacity_region[..word_end]
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect::<Vec<_>>();
    let logical_word_end = allocated_words
        .iter()
        .rposition(|word| *word != 0)
        .map(|index| index + 1)
        .ok_or_else(|| AssetError::InvalidFormat("script payload is entirely zero".to_owned()))?;
    if allocated_words[logical_word_end - 1] != 0xffff {
        return Err(AssetError::InvalidFormat(format!(
            "script logical payload ends in {:04X}, expected FFFF before zero capacity padding",
            allocated_words[logical_word_end - 1]
        )));
    }
    let words = allocated_words[..logical_word_end].to_vec();
    let stream_table = parse_offset_table(&words_to_bytes(&words))?;
    let primary_offsets = derive_primary_offsets(&words)?;
    let secondary_records = parse_runtime_block(
        &working[RUNTIME_BASE..SCRIPT_BASE],
        &primary_offsets,
    )?;
    let metadata = engine_allocations::sc_metadata(entry_id);
    if validate_engine_metadata {
        validate_stream_count(&stream_table, metadata, false)?;
    }
    if validate_engine_metadata {
        validate_primary_count(&primary_offsets, metadata, false)?;
        validate_secondary_count(&secondary_records, metadata, false)?;
    }

    Ok(ParsedScript {
        source_size,
        allocation_size: u32::try_from(allocation_size)
            .map_err(|_| AssetError::InvalidFormat("script allocation exceeds u32".to_owned()))?,
        payload_capacity_bytes: u32::try_from(capacity_region.len()).map_err(|_| {
            AssetError::InvalidFormat("script payload capacity exceeds u32".to_owned())
        })?,
        opaque_footer,
        voice_header,
        words,
        trailing_byte,
        secondary_records,
        metadata,
        stream_table,
        primary_offsets,
    })
}

fn parse_voice_header(input: &[u8]) -> Result<ScriptVoiceHeader, AssetError> {
    if input.len() != VOICE_HEADER_SIZE {
        return Err(AssetError::InvalidFormat(
            "script voice header has the wrong size".to_owned(),
        ));
    }
    if input.starts_with(VOICE_NOT_EXIST) {
        if input[VOICE_NOT_EXIST.len()..].iter().any(|byte| *byte != 0) {
            return Err(AssetError::InvalidFormat(
                "voice_not_exist header has nonzero trailing bytes".to_owned(),
            ));
        }
        return Ok(ScriptVoiceHeader::VoiceNotExist);
    }
    if input[2..].iter().any(|byte| *byte != 0) {
        return Err(AssetError::InvalidFormat(
            "script voice-base header has nonzero bytes after its first u16".to_owned(),
        ));
    }
    Ok(ScriptVoiceHeader::VoiceBase {
        base: u16::from_le_bytes([input[0], input[1]]),
    })
}

fn write_voice_header(
    output: &mut [u8],
    header: &ScriptVoiceHeader,
) -> Result<(), AssetError> {
    if output.len() != VOICE_HEADER_SIZE {
        return Err(AssetError::InvalidProject(
            "internal voice-header output has the wrong size".to_owned(),
        ));
    }
    output.fill(0);
    match header {
        ScriptVoiceHeader::VoiceBase { base } => {
            output[..2].copy_from_slice(&base.to_le_bytes());
        }
        ScriptVoiceHeader::VoiceNotExist => {
            output[..VOICE_NOT_EXIST.len()].copy_from_slice(VOICE_NOT_EXIST);
        }
    }
    Ok(())
}

fn parse_runtime_block(
    runtime: &[u8],
    derived_primary: &[u32],
) -> Result<Vec<ScriptSecondaryRecord>, AssetError> {
    if runtime.len() != SCRIPT_BASE - RUNTIME_BASE || runtime.len() < RUNTIME_HEADER_SIZE {
        return Err(AssetError::InvalidFormat(
            "script runtime block has the wrong size".to_owned(),
        ));
    }
    let primary_offset = read_u32(runtime, 0)?;
    let primary_count = read_u32(runtime, 4)?;
    let secondary_offset = read_u32(runtime, 8)?;
    let secondary_count = read_u32(runtime, 12)?;
    if primary_offset != RUNTIME_HEADER_SIZE as u32 {
        return Err(AssetError::InvalidFormat(format!(
            "script primary table begins at {primary_offset:#x}, expected 0x10"
        )));
    }
    let primary_count_usize = usize::try_from(primary_count).map_err(|_| {
        AssetError::InvalidFormat("script primary count exceeds usize".to_owned())
    })?;
    let primary_end = RUNTIME_HEADER_SIZE
        .checked_add(primary_count_usize.checked_mul(4).ok_or_else(|| {
            AssetError::InvalidFormat("script primary table size overflows".to_owned())
        })?)
        .ok_or_else(|| AssetError::InvalidFormat("script primary table overflows".to_owned()))?;
    let expected_secondary = align_up(primary_end, 0x10)?;
    if usize::try_from(secondary_offset).unwrap_or(usize::MAX) != expected_secondary {
        return Err(AssetError::InvalidFormat(format!(
            "script secondary table begins at {secondary_offset:#x}, expected {expected_secondary:#x}"
        )));
    }
    if primary_end > runtime.len() || expected_secondary > runtime.len() {
        return Err(AssetError::InvalidFormat(
            "script primary table exceeds runtime block".to_owned(),
        ));
    }
    let primary = read_u32_words(&runtime[RUNTIME_HEADER_SIZE..primary_end])?;
    if primary != derived_primary {
        return Err(AssetError::InvalidFormat(format!(
            "script primary table does not match the ordered FFF0 marker chain (stored {}, derived {})",
            primary.len(),
            derived_primary.len()
        )));
    }
    if runtime[primary_end..expected_secondary]
        .iter()
        .any(|byte| *byte != 0)
    {
        return Err(AssetError::InvalidFormat(
            "script primary-table alignment padding is nonzero".to_owned(),
        ));
    }

    let secondary_count_usize = usize::try_from(secondary_count).map_err(|_| {
        AssetError::InvalidFormat("script secondary count exceeds usize".to_owned())
    })?;
    let secondary_size = secondary_count_usize
        .checked_mul(SECONDARY_RECORD_SIZE)
        .ok_or_else(|| AssetError::InvalidFormat("script secondary table overflows".to_owned()))?;
    let secondary_end = expected_secondary
        .checked_add(secondary_size)
        .ok_or_else(|| AssetError::InvalidFormat("script secondary table overflows".to_owned()))?;
    if secondary_end > runtime.len() {
        return Err(AssetError::InvalidFormat(format!(
            "script secondary table range {expected_secondary:#x}..{secondary_end:#x} exceeds runtime block"
        )));
    }
    let mut records = Vec::with_capacity(secondary_count_usize);
    for record in runtime[expected_secondary..secondary_end].chunks_exact(SECONDARY_RECORD_SIZE) {
        let mut words = [0u32; 7];
        for (index, chunk) in record.chunks_exact(4).enumerate() {
            words[index] = u32::from_le_bytes(chunk.try_into().unwrap());
        }
        records.push(ScriptSecondaryRecord { words });
    }
    if runtime[secondary_end..].iter().any(|byte| *byte != 0) {
        return Err(AssetError::InvalidFormat(format!(
            "script runtime tail after {secondary_end:#x} is nonzero"
        )));
    }
    Ok(records)
}

fn write_runtime_block(
    runtime: &mut [u8],
    primary: &[u32],
    secondary: &[ScriptSecondaryRecord],
) -> Result<(), AssetError> {
    if runtime.len() != SCRIPT_BASE - RUNTIME_BASE {
        return Err(AssetError::InvalidProject(
            "internal runtime output has the wrong size".to_owned(),
        ));
    }
    runtime.fill(0);
    let primary_end = RUNTIME_HEADER_SIZE
        .checked_add(primary.len().checked_mul(4).ok_or_else(|| {
            AssetError::InvalidProject("script primary table size overflows".to_owned())
        })?)
        .ok_or_else(|| AssetError::InvalidProject("script primary table overflows".to_owned()))?;
    let secondary_offset = align_up(primary_end, 0x10)
        .map_err(|error| AssetError::InvalidProject(error.to_string()))?;
    let secondary_end = secondary_offset
        .checked_add(secondary.len().checked_mul(SECONDARY_RECORD_SIZE).ok_or_else(|| {
            AssetError::InvalidProject("script secondary table size overflows".to_owned())
        })?)
        .ok_or_else(|| AssetError::InvalidProject("script secondary table overflows".to_owned()))?;
    if secondary_end > runtime.len() {
        return Err(AssetError::InvalidProject(format!(
            "generated script runtime tables end at {secondary_end:#x}, beyond 0x1f80-byte block"
        )));
    }
    write_u32(runtime, 0, RUNTIME_HEADER_SIZE as u32)?;
    write_u32(
        runtime,
        4,
        u32::try_from(primary.len())
            .map_err(|_| AssetError::InvalidProject("too many primary markers".to_owned()))?,
    )?;
    write_u32(
        runtime,
        8,
        u32::try_from(secondary_offset)
            .map_err(|_| AssetError::InvalidProject("secondary offset exceeds u32".to_owned()))?,
    )?;
    write_u32(
        runtime,
        12,
        u32::try_from(secondary.len())
            .map_err(|_| AssetError::InvalidProject("too many secondary records".to_owned()))?,
    )?;
    for (index, value) in primary.iter().enumerate() {
        write_u32(runtime, RUNTIME_HEADER_SIZE + index * 4, *value)?;
    }
    for (record_index, record) in secondary.iter().enumerate() {
        let start = secondary_offset + record_index * SECONDARY_RECORD_SIZE;
        for (word_index, value) in record.words.iter().enumerate() {
            write_u32(runtime, start + word_index * 4, *value)?;
        }
    }
    Ok(())
}

fn derive_primary_offsets(words: &[u16]) -> Result<Vec<u32>, AssetError> {
    let mut candidates = BTreeMap::<u16, Vec<u32>>::new();
    for index in 0..words.len().saturating_sub(1) {
        if words[index] == 0xfff0 && words[index + 1] < 0x8000 {
            candidates
                .entry(words[index + 1])
                .or_default()
                .push(u32::try_from(index * 2).map_err(|_| {
                    AssetError::InvalidFormat("script marker offset exceeds u32".to_owned())
                })?);
        }
    }
    let starts = candidates.get(&0).ok_or_else(|| {
        AssetError::InvalidFormat("script payload has no FFF0 0000 primary marker".to_owned())
    })?;
    let mut best: Option<Vec<u32>> = None;
    let mut ambiguous = false;
    for start in starts {
        let mut chain = vec![*start];
        let mut previous = *start;
        let mut index = 1u16;
        while let Some(offsets) = candidates.get(&index) {
            let Some(next) = offsets.iter().copied().find(|offset| *offset > previous) else {
                break;
            };
            chain.push(next);
            previous = next;
            index = index.checked_add(1).ok_or_else(|| {
                AssetError::InvalidFormat("script primary marker index overflows u16".to_owned())
            })?;
        }
        match &best {
            None => best = Some(chain),
            Some(current) if chain.len() > current.len() => {
                best = Some(chain);
                ambiguous = false;
            }
            Some(current) if chain.len() == current.len() && chain.as_slice() != current.as_slice() => {
                ambiguous = true;
            }
            _ => {}
        }
    }
    let best = best.ok_or_else(|| {
        AssetError::InvalidFormat("script primary marker chain is empty".to_owned())
    })?;
    if ambiguous {
        return Err(AssetError::InvalidFormat(
            "script payload has multiple equally long primary marker chains".to_owned(),
        ));
    }
    Ok(best)
}

fn parse_text_ranges(
    words: &[u16],
    primary_offsets: &[u32],
) -> Result<Vec<ScriptTextRange>, AssetError> {
    let mut ranges = Vec::with_capacity(primary_offsets.len());
    for (expected_index, &byte_offset) in primary_offsets.iter().enumerate() {
        let start = usize::try_from(byte_offset)
            .map_err(|_| AssetError::InvalidFormat("script text marker offset exceeds usize".to_owned()))?
            / 2;
        if start + 2 > words.len()
            || words[start] != 0xfff0
            || words[start + 1] != u16::try_from(expected_index).unwrap_or(u16::MAX)
        {
            return Err(AssetError::InvalidFormat(format!(
                "primary marker {expected_index} at {byte_offset:#x} is not FFF0 {expected_index:04X}"
            )));
        }
        let speaker_start = start + 2;
        let speaker_end = words[speaker_start..]
            .iter()
            .position(|value| *value == 0xffff)
            .map(|relative| speaker_start + relative)
            .ok_or_else(|| AssetError::InvalidFormat(format!(
                "primary marker {expected_index} has no FFFF speaker terminator"
            )))?;
        if words[speaker_start..speaker_end]
            .iter()
            .any(|value| usize::from(*value) >= charset::GLYPH_COUNT)
        {
            return Err(AssetError::InvalidFormat(format!(
                "primary marker {expected_index} speaker contains a non-glyph word"
            )));
        }

        // FFFE is a page/line-completion boundary, not the end of the whole
        // primary dialogue. The first page may be empty. Additional pages are
        // recognized only when an immediately following glyph-only run ends in
        // another FFFE; this avoids consuming arbitrary command/data words.
        let first_page_start = speaker_end + 1;
        let first_page_end = words[first_page_start..]
            .iter()
            .position(|value| *value == 0xfffe)
            .map(|relative| first_page_start + relative)
            .ok_or_else(|| AssetError::InvalidFormat(format!(
                "primary marker {expected_index} has no FFFE page terminator"
            )))?;
        if words[first_page_start..first_page_end]
            .iter()
            .any(|value| usize::from(*value) >= charset::GLYPH_COUNT)
        {
            return Err(AssetError::InvalidFormat(format!(
                "primary marker {expected_index} first page contains a non-glyph word"
            )));
        }

        let mut pages = vec![ScriptPageRange {
            start: first_page_start,
            end: first_page_end,
        }];
        let mut cursor = first_page_end + 1;
        loop {
            let page_start = cursor;
            while cursor < words.len() && usize::from(words[cursor]) < charset::GLYPH_COUNT {
                cursor += 1;
            }
            if cursor > page_start && cursor < words.len() && words[cursor] == 0xfffe {
                pages.push(ScriptPageRange {
                    start: page_start,
                    end: cursor,
                });
                cursor += 1;
            } else {
                cursor = page_start;
                break;
            }
        }

        ranges.push(ScriptTextRange {
            marker_index: u32::try_from(expected_index).unwrap(),
            byte_offset,
            speaker_start,
            speaker_end,
            pages,
            end: cursor,
        });
    }
    Ok(ranges)
}

fn build_source_document(
    entry_id: u32,
    words: &[u16],
    stream_table: &ScriptOffsetTableAnnotation,
    primary_offsets: &[u32],
    secondary_records: &[ScriptSecondaryRecord],
) -> Result<ScriptSourceDocument, AssetError> {
    let table_bytes = usize::try_from(stream_table.table_bytes)
        .map_err(|_| AssetError::InvalidFormat("script stream table exceeds usize".to_owned()))?;
    let table_words = table_bytes / 2;
    if table_words > words.len() {
        return Err(AssetError::InvalidFormat(
            "script stream table exceeds payload words".to_owned(),
        ));
    }

    let text_ranges = parse_text_ranges(words, primary_offsets)?;
    let mut text_by_start = BTreeMap::<usize, ScriptTextRange>::new();
    let mut boundaries = BTreeSet::<usize>::new();
    let mut labels = BTreeMap::<usize, Vec<String>>::new();
    boundaries.insert(table_words);
    boundaries.insert(words.len());

    for entry in &stream_table.entries {
        let offset = usize::try_from(entry.offset)
            .map_err(|_| AssetError::InvalidFormat("stream offset exceeds usize".to_owned()))?;
        let word = offset / 2;
        if word < table_words || word > words.len() {
            return Err(AssetError::InvalidFormat(format!(
                "stream {} points outside relocatable body",
                entry.index
            )));
        }
        boundaries.insert(word);
        labels
            .entry(word)
            .or_default()
            .push(format!("stream_{:04}", entry.index));
    }

    for range in &text_ranges {
        let start = usize::try_from(range.byte_offset).unwrap() / 2;
        boundaries.insert(start);
        boundaries.insert(range.end);
        text_by_start.insert(start, range.clone());
    }

    let payload_bytes = words.len().checked_mul(2).ok_or_else(|| {
        AssetError::InvalidFormat("script payload byte size overflows".to_owned())
    })?;
    let mut source_secondary = Vec::with_capacity(secondary_records.len());
    for (record_index, record) in secondary_records.iter().enumerate() {
        let mut fields = Vec::with_capacity(7);
        for (field_index, value) in record.words.iter().copied().enumerate() {
            if is_secondary_payload_offset(value, table_bytes, payload_bytes) {
                let offset = usize::try_from(value).unwrap();
                let word = offset / 2;
                let name = format!("secondary_{record_index:03}_{field_index}_target");
                boundaries.insert(word);
                labels.entry(word).or_default().push(name.clone());
                fields.push(ScriptSourceValue::Label { name });
            } else {
                fields.push(ScriptSourceValue::Immediate { value });
            }
        }
        let fields: [ScriptSourceValue; 7] = fields.try_into().map_err(|_| {
            AssetError::InvalidFormat("secondary record did not contain seven fields".to_owned())
        })?;
        source_secondary.push(ScriptSourceSecondaryRecord { fields });
    }

    let ordered = boundaries.into_iter().collect::<Vec<_>>();
    let mut nodes = Vec::new();
    let mut cursor_index = 0usize;
    while cursor_index + 1 < ordered.len() {
        let start = ordered[cursor_index];
        let end = ordered[cursor_index + 1];
        cursor_index += 1;
        if start == end {
            continue;
        }
        let node_labels = labels.remove(&start).unwrap_or_default();
        if let Some(range) = text_by_start.get(&start) {
            if end != range.end {
                return Err(AssetError::InvalidFormat(format!(
                    "a relocation label splits primary dialogue {}",
                    range.marker_index
                )));
            }
            nodes.push(ScriptSourceNode::Dialogue {
                labels: node_labels,
                marker_index: range.marker_index,
                speaker: charset::decode_slice(&words[range.speaker_start..range.speaker_end])?,
                speaker_source_glyphs: words[range.speaker_start..range.speaker_end].to_vec(),
                pages: range
                    .pages
                    .iter()
                    .map(|page| {
                        let source_glyphs = words[page.start..page.end].to_vec();
                        Ok(ScriptDialoguePage {
                            text: charset::decode_slice(&source_glyphs)?,
                            source_glyphs,
                        })
                    })
                    .collect::<Result<Vec<_>, AssetError>>()?,
            });
        } else {
            nodes.push(ScriptSourceNode::Raw {
                labels: node_labels,
                words: words[start..end].to_vec(),
            });
        }
    }
    if !labels.is_empty() {
        return Err(AssetError::InvalidFormat(
            "script has relocation labels at the end of the payload".to_owned(),
        ));
    }

    Ok(ScriptSourceDocument {
        document_version: SOURCE_DOCUMENT_VERSION,
        entry_id,
        charset: "rz-jis-x0208-v1".to_owned(),
        stream_count: u32::try_from(stream_table.entries.len()).unwrap_or(u32::MAX),
        nodes,
        secondary_records: source_secondary,
        relocation_model: "all located VM branch targets are stream IDs; stream offsets and primary marker offsets are directly proven, while corpus-classified secondary payload addresses are regenerated from labels"
            .to_owned(),
    })
}

fn is_secondary_payload_offset(value: u32, table_bytes: usize, payload_bytes: usize) -> bool {
    let value = usize::try_from(value).unwrap_or(usize::MAX);
    value >= table_bytes.max(0x100) && value < payload_bytes && value % 2 == 0
}

fn assemble_source_document(
    entry_id: u32,
    source: &ScriptSourceDocument,
    charset_map: &charset::CharsetMap,
) -> Result<(Vec<u16>, Vec<ScriptSecondaryRecord>), AssetError> {
    if source.document_version != SOURCE_DOCUMENT_VERSION
        || source.entry_id != entry_id
        || source.charset != charset_map.id()
    {
        return Err(AssetError::InvalidProject(
            "editable script document version, entry ID, or charset does not match script-meta.json".to_owned(),
        ));
    }
    let stream_count = usize::try_from(source.stream_count)
        .map_err(|_| AssetError::InvalidProject("stream count exceeds usize".to_owned()))?;
    if stream_count == 0 || stream_count > usize::from(u16::MAX) {
        return Err(AssetError::InvalidProject(
            "editable script stream count is invalid".to_owned(),
        ));
    }
    let table_words = stream_count.checked_mul(2).ok_or_else(|| {
        AssetError::InvalidProject("stream table word count overflows".to_owned())
    })?;
    let mut words = vec![0u16; table_words];
    let mut labels = BTreeMap::<String, u32>::new();
    let mut expected_marker = 0u32;

    for node in &source.nodes {
        let byte_offset = u32::try_from(words.len().checked_mul(2).ok_or_else(|| {
            AssetError::InvalidProject("script source address overflows".to_owned())
        })?)
        .map_err(|_| AssetError::InvalidProject("script source exceeds u32".to_owned()))?;
        for label in node.labels() {
            if label.is_empty() || labels.insert(label.clone(), byte_offset).is_some() {
                return Err(AssetError::InvalidProject(format!(
                    "editable script has an empty or duplicate label {label:?}"
                )));
            }
        }
        match node {
            ScriptSourceNode::Raw { words: raw, .. } => words.extend_from_slice(raw),
            ScriptSourceNode::Dialogue {
                marker_index,
                speaker,
                speaker_source_glyphs,
                pages,
                ..
            } => {
                if *marker_index != expected_marker {
                    return Err(AssetError::InvalidProject(format!(
                        "dialogue marker {marker_index} appears where marker {expected_marker} is required"
                    )));
                }
                let marker = u16::try_from(*marker_index).map_err(|_| {
                    AssetError::InvalidProject("dialogue marker exceeds u16".to_owned())
                })?;
                words.push(0xfff0);
                words.push(marker);
                if pages.is_empty() {
                    return Err(AssetError::InvalidProject(format!(
                        "dialogue marker {marker_index} has no pages"
                    )));
                }
                words.extend(encode_span_preserving_source(
                    speaker,
                    speaker_source_glyphs,
                    charset_map,
                )?);
                words.push(0xffff);
                for page in pages {
                    words.extend(encode_span_preserving_source(
                        &page.text,
                        &page.source_glyphs,
                        charset_map,
                    )?);
                    words.push(0xfffe);
                }
                expected_marker = expected_marker.checked_add(1).ok_or_else(|| {
                    AssetError::InvalidProject("dialogue marker count overflows".to_owned())
                })?;
            }
        }
    }

    for stream_id in 0..stream_count {
        let label = format!("stream_{stream_id:04}");
        let offset = *labels.get(&label).ok_or_else(|| {
            AssetError::InvalidProject(format!("editable script is missing label {label}"))
        })?;
        words[stream_id * 2] = offset as u16;
        words[stream_id * 2 + 1] = (offset >> 16) as u16;
    }

    let mut secondary = Vec::with_capacity(source.secondary_records.len());
    for record in &source.secondary_records {
        let mut values = [0u32; 7];
        for (index, field) in record.fields.iter().enumerate() {
            values[index] = match field {
                ScriptSourceValue::Immediate { value } => *value,
                ScriptSourceValue::Label { name } => *labels.get(name).ok_or_else(|| {
                    AssetError::InvalidProject(format!(
                        "secondary record references missing label {name:?}"
                    ))
                })?,
            };
        }
        secondary.push(ScriptSecondaryRecord { words: values });
    }
    Ok((words, secondary))
}

fn encode_span_preserving_source(
    text: &str,
    source_glyphs: &[u16],
    charset_map: &charset::CharsetMap,
) -> Result<Vec<u16>, AssetError> {
    if charset_map.decode_slice(source_glyphs)? == text {
        Ok(source_glyphs.to_vec())
    } else {
        charset_map.encode_string(text)
    }
}

fn parse_offset_table(script: &[u8]) -> Result<ScriptOffsetTableAnnotation, AssetError> {
    if script.len() < 4 {
        return Err(AssetError::InvalidFormat(
            "script payload is shorter than its first stream offset".to_owned(),
        ));
    }
    let first = u32::from_le_bytes(script[..4].try_into().unwrap());
    let table_bytes = usize::try_from(first)
        .map_err(|_| AssetError::InvalidFormat("stream table size exceeds usize".to_owned()))?;
    if table_bytes < 4 || table_bytes % 4 != 0 || table_bytes > script.len() {
        return Err(AssetError::InvalidFormat(format!(
            "script stream table has invalid byte size {first:#x}"
        )));
    }
    let count = table_bytes / 4;
    let mut entries = Vec::with_capacity(count);
    for index in 0..count {
        let offset = u32::from_le_bytes(
            script[index * 4..index * 4 + 4]
                .try_into()
                .expect("four-byte script stream offset"),
        );
        let offset_usize = usize::try_from(offset).unwrap_or(usize::MAX);
        if offset_usize >= script.len() || offset_usize % 2 != 0 {
            return Err(AssetError::InvalidFormat(format!(
                "script stream table entry {index} has invalid offset {offset:#x}"
            )));
        }
        entries.push(ScriptOffsetAnnotation {
            index: u32::try_from(index).unwrap(),
            offset,
            even_aligned: true,
            inside_payload: true,
        });
    }
    Ok(ScriptOffsetTableAnnotation {
        table_bytes: first,
        entries,
    })
}

fn metadata_annotation(metadata: ScMetadata) -> ScriptEngineMetadataAnnotation {
    ScriptEngineMetadataAnnotation {
        address: "eboot.bin.elf:0x810F9B1C + entry_id * 0x0C".to_owned(),
        stream_global_base: metadata.stream_base,
        stream_count: metadata.stream_count,
        primary_global_base: metadata.primary_base,
        primary_count: metadata.primary_count,
        secondary_global_base: metadata.secondary_base,
        secondary_count: metadata.secondary_count,
        meaning: "Global runtime ranges and expected local counts; not embedded script content"
            .to_owned(),
    }
}

fn validate_metadata_annotation(
    annotation: Option<&ScriptEngineMetadataAnnotation>,
    metadata: Option<ScMetadata>,
) -> Result<(), AssetError> {
    match (annotation, metadata) {
        (Some(annotation), Some(metadata)) => {
            let actual = (
                annotation.stream_global_base,
                annotation.stream_count,
                annotation.primary_global_base,
                annotation.primary_count,
                annotation.secondary_global_base,
                annotation.secondary_count,
            );
            let expected = (
                metadata.stream_base,
                metadata.stream_count,
                metadata.primary_base,
                metadata.primary_count,
                metadata.secondary_base,
                metadata.secondary_count,
            );
            if actual != expected {
                return Err(AssetError::InvalidProject(
                    "script engine_metadata was edited and no longer matches the executable table"
                        .to_owned(),
                ));
            }
        }
        (None, Some(_)) => {
            return Err(AssetError::InvalidProject(
                "script document is missing executable-resident engine_metadata".to_owned(),
            ));
        }
        (Some(_), None) => {
            return Err(AssetError::InvalidProject(
                "script document has engine_metadata for an unknown entry ID".to_owned(),
            ));
        }
        (None, None) => {}
    }
    Ok(())
}

fn validate_stream_count(
    table: &ScriptOffsetTableAnnotation,
    metadata: Option<ScMetadata>,
    project: bool,
) -> Result<(), AssetError> {
    if let Some(metadata) = metadata {
        if table.entries.len() != usize::from(metadata.stream_count) {
            return count_error(
                project,
                format!(
                    "script stream count is {}, executable expects {}",
                    table.entries.len(),
                    metadata.stream_count
                ),
            );
        }
    }
    Ok(())
}

fn validate_primary_count(
    primary: &[u32],
    metadata: Option<ScMetadata>,
    project: bool,
) -> Result<(), AssetError> {
    if let Some(metadata) = metadata {
        if primary.len() != usize::from(metadata.primary_count) {
            return count_error(
                project,
                format!(
                    "script primary marker count is {}, executable expects {}",
                    primary.len(),
                    metadata.primary_count
                ),
            );
        }
    }
    Ok(())
}

fn validate_secondary_count(
    secondary: &[ScriptSecondaryRecord],
    metadata: Option<ScMetadata>,
    project: bool,
) -> Result<(), AssetError> {
    if let Some(metadata) = metadata {
        if secondary.len() != usize::from(metadata.secondary_count) {
            return count_error(
                project,
                format!(
                    "script secondary record count is {}, executable expects {}",
                    secondary.len(),
                    metadata.secondary_count
                ),
            );
        }
    }
    Ok(())
}

fn count_error(project: bool, message: String) -> Result<(), AssetError> {
    if project {
        Err(AssetError::InvalidProject(message))
    } else {
        Err(AssetError::InvalidFormat(message))
    }
}

fn primary_annotation(offsets: Vec<u32>) -> ScriptPrimaryMarkerAnnotation {
    ScriptPrimaryMarkerAnnotation {
        marker_word: "FFF0".to_owned(),
        derived_count: u32::try_from(offsets.len()).unwrap_or(u32::MAX),
        offsets,
        generation: "Longest strictly increasing chain of FFF0 <index> markers, beginning at index 0 and continuing 1, 2, ...; verified against all 89 original SC entries and the ELF count table"
            .to_owned(),
    }
}

fn resource_routing() -> ScriptResourceRoutingAnnotation {
    ScriptResourceRoutingAnnotation {
        embedded_image_bytes: false,
        archives: vec![
            ScriptResourceArchiveAnnotation {
                archive: "bk.cpk".to_owned(),
                role: "background and event-image resources referenced by compiled script controls"
                    .to_owned(),
                evidence: vec![
                    "0xFF48/0xFF49 interpreter paths reach FUN_81053CDA/FUN_81053E80"
                        .to_owned(),
                    "the BK path validates resource IDs below 0x146".to_owned(),
                ],
            },
            ScriptResourceArchiveAnnotation {
                archive: "bsf.cpk".to_owned(),
                role: "streamed character/effect surfaces selected by layer groups 2..5"
                    .to_owned(),
                evidence: vec![
                    "0xFF33 reaches FUN_81008300 then FUN_81020316".to_owned(),
                    "runtime group 1 selects BK; groups 2..5 select BSF".to_owned(),
                ],
            },
        ],
    }
}

fn limitations() -> Vec<String> {
    vec![
        "Exact compiler source cannot be recovered byte-for-byte: comments, macro names, local identifiers, symbolic labels, and the original compiler syntax are absent from the executable payload"
            .to_owned(),
        "script.json is a lossless relocatable source-equivalent IR: dialogue speakers and ordered FFFE-delimited pages are typed Unicode, control/data not yet assigned proven semantics remains in raw u16 nodes, and every engine-consumed payload address is represented by a label"
            .to_owned(),
        "Speaker and dialogue pages may change encoded length; build preserves page boundaries and regenerates the stream table, primary marker table, and payload-address fields in secondary records"
            .to_owned(),
        "The seven secondary-record fields are not given speculative semantic names; corpus-wide separation identifies payload addresses versus small immediate values without claiming their higher-level purpose"
            .to_owned(),
        "The zero-filled gap before the 16-byte opaque footer is reusable payload capacity; the footer is preserved at allocation end because no located consumer explains how to regenerate it"
            .to_owned(),
        "The executable's fixed sector allocation is a capacity limit rather than a relocation limit; output larger than the allocated entry requires patching the SC sector-count table in eboot.bin.elf"
            .to_owned(),
    ]
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
            "script opaque_footer_hex has odd length".to_owned(),
        ));
    }
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len() / 2);
    for index in (0..bytes.len()).step_by(2) {
        let high = hex_nibble(bytes[index]).ok_or_else(|| {
            AssetError::InvalidProject(format!(
                "script opaque_footer_hex has invalid character at {index}"
            ))
        })?;
        let low = hex_nibble(bytes[index + 1]).ok_or_else(|| {
            AssetError::InvalidProject(format!(
                "script opaque_footer_hex has invalid character at {}",
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

fn words_to_bytes(words: &[u16]) -> Vec<u8> {
    let mut output = Vec::with_capacity(words.len() * 2);
    for word in words {
        output.extend_from_slice(&word.to_le_bytes());
    }
    output
}

fn align_up(value: usize, alignment: usize) -> Result<usize, AssetError> {
    value
        .checked_add(alignment - 1)
        .map(|value| value & !(alignment - 1))
        .ok_or_else(|| AssetError::InvalidFormat("script alignment overflows".to_owned()))
}

fn read_u32(input: &[u8], offset: usize) -> Result<u32, AssetError> {
    let bytes = input
        .get(offset..offset + 4)
        .ok_or_else(|| AssetError::InvalidFormat("script u32 field is truncated".to_owned()))?;
    Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
}

fn read_u32_words(input: &[u8]) -> Result<Vec<u32>, AssetError> {
    if input.len() % 4 != 0 {
        return Err(AssetError::InvalidFormat(
            "script u32 section is not aligned".to_owned(),
        ));
    }
    Ok(input
        .chunks_exact(4)
        .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()))
        .collect())
}

fn write_u32(input: &mut [u8], offset: usize, value: u32) -> Result<(), AssetError> {
    let target = input.get_mut(offset..offset + 4).ok_or_else(|| {
        AssetError::InvalidProject("generated script u32 field is out of range".to_owned())
    })?;
    target.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_input(offsets: &[u32], extra_words: &[u16]) -> Vec<u8> {
        let allocation = SCRIPT_BASE + 0x80;
        let mut words = Vec::new();
        for offset in offsets {
            words.extend_from_slice(&[
                (*offset & 0xffff) as u16,
                ((*offset >> 16) & 0xffff) as u16,
            ]);
        }
        words.extend_from_slice(extra_words);
        let primary = derive_primary_offsets(&words).unwrap();
        let mut input = vec![0u8; SCRIPT_BASE];
        write_voice_header(
            &mut input[..VOICE_HEADER_SIZE],
            &ScriptVoiceHeader::VoiceBase { base: 123 },
        )
        .unwrap();
        write_runtime_block(&mut input[RUNTIME_BASE..SCRIPT_BASE], &primary, &[]).unwrap();
        for word in words {
            input.extend_from_slice(&word.to_le_bytes());
        }
        input.resize(allocation - OPAQUE_FOOTER_SIZE, 0);
        input.extend_from_slice(&[
            0x21, 0x67, 0x27, 0x47, 0xd9, 0x5d, 0x8e, 0x69,
            0xee, 0xb9, 0x13, 0x86, 0xf0, 0xc2, 0x63, 0xaa,
        ]);
        input
    }

    fn round_trip(input: &[u8], name: &str) -> Vec<u8> {
        let root = std::env::temp_dir().join(format!(
            "rz-script-1-0-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let kind = decode(input, &root, "00000", 999, input.len()).unwrap();
        let document = match kind {
            AssetKind::Script { document } => document,
            _ => panic!("unexpected asset kind"),
        };
        let encoded = encode(&root.join(document)).unwrap();
        let _ = fs::remove_dir_all(root);
        encoded
    }

    #[test]
    fn runtime_block_is_generated_without_skeleton() {
        let input = synthetic_input(
            &[8, 8],
            &[0xfff0, 0, 0xffff, 0xfffe, 0xfef4, 0, 0xffff],
        );
        assert_eq!(round_trip(&input, "generated-runtime"), input);
    }

    #[test]
    fn script_preserves_aliases_and_non_monotonic_stream_offsets() {
        let input = synthetic_input(
            &[0x18, 0x10, 0x18, 0x1c],
            &[0xfff0, 0, 0xffff, 0xfffe, 0x1111, 0x2222, 0x3333, 0xffff],
        );
        assert_eq!(round_trip(&input, "aliases"), input);
    }

    #[test]
    fn variable_length_text_relocates_later_streams() {
        let words = vec![
            8, 0, 26, 0,
            0xfff0, 0, 0x013b, 0x0152, 0x016d, 0xffff,
            0x016e, 0x0162, 0xfffe,
            0xfef4, 0xffff,
        ];
        let table = parse_offset_table(&words_to_bytes(&words)).unwrap();
        let offsets = derive_primary_offsets(&words).unwrap();
        let mut source = build_source_document(7, &words, &table, &offsets, &[]).unwrap();
        let dialogue = source.nodes.iter_mut().find_map(|node| match node {
            ScriptSourceNode::Dialogue { speaker, pages, .. } => Some((speaker, pages)),
            _ => None,
        }).unwrap();
        assert_eq!(dialogue.0, "スバル");
        assert_eq!(dialogue.1[0].text, "レム");
        dialogue.1[0].text = "レムム".to_owned();
        let (rebuilt, secondary) =
            assemble_source_document(7, &source, charset::default_map()).unwrap();
        assert!(secondary.is_empty());
        let relocated = parse_offset_table(&words_to_bytes(&rebuilt)).unwrap();
        assert_eq!(relocated.entries[0].offset, 8);
        assert_eq!(relocated.entries[1].offset, 28);
        assert_eq!(rebuilt.len(), words.len() + 1);
    }

    #[test]
    fn multi_page_dialogue_round_trips_exactly() {
        let words = vec![
            4, 0,
            0xfff0, 0, 0x013b, 0xffff,
            0x016e, 0x0162, 0xfffe,
            0x013b, 0xfffe,
            0x016e, 0xfffe,
            0xfef4, 0xffff,
        ];
        let table = parse_offset_table(&words_to_bytes(&words)).unwrap();
        let offsets = derive_primary_offsets(&words).unwrap();
        let source = build_source_document(86, &words, &table, &offsets, &[]).unwrap();
        let pages = source.nodes.iter().find_map(|node| match node {
            ScriptSourceNode::Dialogue { pages, .. } => Some(pages),
            _ => None,
        }).unwrap();
        assert_eq!(
            pages.iter().map(|page| page.text.as_str()).collect::<Vec<_>>(),
            vec!["レム", "ス", "レ"]
        );
        let (rebuilt, secondary) =
            assemble_source_document(86, &source, charset::default_map()).unwrap();
        assert!(secondary.is_empty());
        assert_eq!(rebuilt, words);
    }

    #[test]
    fn unchanged_charset_alias_preserves_original_glyph_id() {
        let words = vec![
            4, 0,
            0xfff0, 0, 0xffff,
            0x001b, 0xfffe,
            0xfef4, 0xffff,
        ];
        let table = parse_offset_table(&words_to_bytes(&words)).unwrap();
        let offsets = derive_primary_offsets(&words).unwrap();
        let source = build_source_document(86, &words, &table, &offsets, &[]).unwrap();
        let pages = source.nodes.iter().find_map(|node| match node {
            ScriptSourceNode::Dialogue { pages, .. } => Some(pages),
            _ => None,
        }).unwrap();
        assert_eq!(pages[0].text.as_str(), "ー");
        assert_eq!(pages[0].source_glyphs.as_slice(), &[0x001b]);
        let (rebuilt, _) =
            assemble_source_document(86, &source, charset::default_map()).unwrap();
        assert_eq!(rebuilt, words);
    }

    #[test]
    fn glyph_run_without_fffe_remains_raw() {
        let words = vec![
            4, 0,
            0xfff0, 0, 0xffff,
            0x016e, 0xfffe,
            0x013b, 0xffff, 0xfef4, 0xffff,
        ];
        let table = parse_offset_table(&words_to_bytes(&words)).unwrap();
        let offsets = derive_primary_offsets(&words).unwrap();
        let source = build_source_document(5, &words, &table, &offsets, &[]).unwrap();
        let dialogue = source.nodes.iter().find_map(|node| match node {
            ScriptSourceNode::Dialogue { pages, .. } => Some(pages),
            _ => None,
        }).unwrap();
        assert_eq!(dialogue.len(), 1);
        assert_eq!(dialogue[0].text, "レ");
        assert!(source.nodes.iter().any(|node| matches!(
            node,
            ScriptSourceNode::Raw { words, .. }
                if words.starts_with(&[0x013b, 0xffff])
        )));
        let (rebuilt, _) = assemble_source_document(5, &source, charset::default_map()).unwrap();
        assert_eq!(rebuilt, words);
    }

    #[test]
    fn secondary_records_are_exactly_seven_u32_words() {
        let record = ScriptSecondaryRecord {
            words: [2, 0xfef4, 0xff0c, 0, 0, 0, 0],
        };
        let mut runtime = vec![0u8; SCRIPT_BASE - RUNTIME_BASE];
        write_runtime_block(&mut runtime, &[8], &[record.clone()]).unwrap();
        assert_eq!(parse_runtime_block(&runtime, &[8]).unwrap(), vec![record]);
    }
}
