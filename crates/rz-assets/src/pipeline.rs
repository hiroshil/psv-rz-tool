use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::error::Error;
use std::fs::{self, File};
use std::io::{Read, Seek};
use sha2::{Digest, Sha256};
use serde::Deserialize;
use std::path::{Path, PathBuf};

use cri_archive_lib::cpk::encrypt::data::DummyDecryptor;
use cri_archive_lib::cpk::reader::{CpkMetadata, CpkReader};
use cri_archive_lib::cpk::writer::{
    CpkIndexMode, CpkInputFile, CpkWriter, CpkWriterOptions, CpkWriterProfile,
};

use crate::codec::{self, DecodeContext};
use crate::eboot::{self, EbootPatchReport, LtEbootPatchReport, LtPatchPlan, ScEntryPatch, ScPatchPlan, SC_ENTRY_COUNT};
use crate::engine_allocations;
use crate::error::AssetError;
use crate::manifest::{
    AssetEntry, AssetKind, CpkProject, IndexMode, ProjectManifest, ProjectMode,
    ProjectSource, PROJECT_FILE_NAME, PROJECT_SCHEMA_VERSION,
};

#[derive(Debug, Clone, Default)]
pub struct ExtractOptions {
    pub raw_only: bool,
    pub debug_script_ir: bool,
    pub charset_map: Option<PathBuf>,
    pub allocation_map: Option<PathBuf>,
    pub use_stock_charset: bool,
    pub use_stock_allocation: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WrapMode {
    /// Default translator-facing wrap: keep whitespace-delimited words intact and
    /// wrap before the next word would exceed the pixel limit. If a single word
    /// is wider than the limit, fall back to glyph-level splitting only for that
    /// impossible word.
    Word,
    /// Legacy behavior from the first fixed-width implementation: break as soon
    /// as adding the next glyph would exceed the pixel limit, even if this cuts
    /// through a word.
    Legacy,
}

impl Default for WrapMode {
    fn default() -> Self {
        Self::Word
    }
}

#[derive(Debug, Clone, Default)]
pub struct BuildOptions {
    pub eboot_in: Option<PathBuf>,
    pub eboot_out: Option<PathBuf>,
    pub force: bool,
    pub charset_map: Option<PathBuf>,
    pub wrap_width_px: Option<u32>,
    pub wrap_width_table: Option<PathBuf>,
    /// Number of physical text rows allowed per dialogue page.
    pub wrap_rows: Option<u32>,
    pub wrap_mode: Option<WrapMode>,
}

const DEFAULT_WRAP_WIDTH_PX: u32 = 528;
const DEFAULT_WRAP_ROWS: u32 = 3;


#[derive(Debug, Clone, Copy)]
pub struct ExtractReport {
    pub files: u32,
    pub editable: bool,
}

#[derive(Debug, Clone)]
pub struct BuildReport {
    pub files: u32,
    pub output_size: u64,
    pub eboot_patch: Option<EbootPatchReport>,
    pub lt_eboot_patch: Option<LtEbootPatchReport>,
}

struct BuildOutcome {
    report: BuildReport,
    sc_patch_plan: Option<ScPatchPlan>,
    lt_patch_plan: Option<LtPatchPlan>,
    portable_dialogue_metadata: Option<codec::script::ScenarioDialogueMetadataDocument>,
}

#[derive(Debug, Deserialize)]
struct ScAllocationMapFile {
    entries: Vec<ScAllocationMapEntryFile>,
}

#[derive(Debug, Deserialize)]
struct ScAllocationMapEntryFile {
    entry_id: u32,
    allocation_size: usize,
    stream_base: Option<u16>,
    stream_count: Option<u16>,
    primary_base: Option<u16>,
    primary_count: Option<u16>,
    secondary_base: Option<u16>,
    secondary_count: Option<u16>,
}

#[derive(Debug, Deserialize)]
struct LtAllocationMapFile {
    format: String,
    version: u32,
    glyph_count: u32,
    allocation_size: usize,
    sector_count: u32,
}

#[derive(Debug, Clone, Copy)]
struct LtExtractAllocation {
    glyph_count: usize,
    allocation_size: usize,
}

fn load_lt_allocation_map(path: &Path) -> Result<LtExtractAllocation, AssetError> {
    let document: LtAllocationMapFile = serde_json::from_slice(&fs::read(path)?)?;
    if document.format != "rz-lt-allocation-map" || document.version != 1 {
        return Err(AssetError::InvalidProject(
            "LT allocation map must use format=rz-lt-allocation-map version=1".to_owned(),
        ));
    }
    let glyph_count = usize::try_from(document.glyph_count).map_err(|_| {
        AssetError::InvalidProject("LT allocation map glyph_count exceeds usize".to_owned())
    })?;
    let sector_count = usize::try_from(document.sector_count).map_err(|_| {
        AssetError::InvalidProject("LT allocation map sector_count exceeds usize".to_owned())
    })?;
    let sector_bytes = sector_count.checked_mul(engine_allocations::SECTOR_SIZE).ok_or_else(|| {
        AssetError::InvalidProject("LT allocation map sector size overflows usize".to_owned())
    })?;
    if sector_bytes != document.allocation_size {
        return Err(AssetError::InvalidProject(format!(
            "LT allocation map sector_count {:#x} allocates {sector_bytes:#x} bytes, but allocation_size is {:#x}",
            document.sector_count, document.allocation_size
        )));
    }
    Ok(LtExtractAllocation {
        glyph_count,
        allocation_size: document.allocation_size,
    })
}

#[derive(Debug, Clone, Copy, Default)]
struct ScExtractEntry {
    allocation_size: usize,
    metadata: Option<engine_allocations::ScMetadata>,
}

fn load_sc_allocation_map(path: &Path) -> Result<[ScExtractEntry; SC_ENTRY_COUNT], AssetError> {
    let document: ScAllocationMapFile = serde_json::from_slice(&fs::read(path)?)?;
    let mut entries = [ScExtractEntry::default(); SC_ENTRY_COUNT];
    let mut seen = [false; SC_ENTRY_COUNT];
    for entry in document.entries {
        let index = usize::try_from(entry.entry_id).map_err(|_| {
            AssetError::InvalidProject("SC allocation map entry ID exceeds usize".to_owned())
        })?;
        if index >= SC_ENTRY_COUNT || seen[index] {
            return Err(AssetError::InvalidProject(format!(
                "SC allocation map has invalid or duplicate entry ID {}",
                entry.entry_id
            )));
        }
        if entry.allocation_size <= 0x2000 || entry.allocation_size % engine_allocations::SECTOR_SIZE != 0 {
            return Err(AssetError::InvalidProject(format!(
                "SC allocation map entry {} has invalid allocation size {:#x}",
                entry.entry_id, entry.allocation_size
            )));
        }
        let metadata_fields = [
            entry.stream_base,
            entry.stream_count,
            entry.primary_base,
            entry.primary_count,
            entry.secondary_base,
            entry.secondary_count,
        ];
        let metadata = if metadata_fields.iter().all(Option::is_some) {
            Some(engine_allocations::ScMetadata {
                stream_base: entry.stream_base.unwrap(),
                stream_count: entry.stream_count.unwrap(),
                primary_base: entry.primary_base.unwrap(),
                primary_count: entry.primary_count.unwrap(),
                secondary_base: entry.secondary_base.unwrap(),
                secondary_count: entry.secondary_count.unwrap(),
            })
        } else if metadata_fields.iter().any(Option::is_some) {
            return Err(AssetError::InvalidProject(format!(
                "SC allocation map entry {} has partial executable metadata; provide all six stream/primary/secondary fields or none",
                entry.entry_id
            )));
        } else {
            None
        };
        entries[index] = ScExtractEntry {
            allocation_size: entry.allocation_size,
            metadata,
        };
        seen[index] = true;
    }
    if seen.iter().any(|value| !*value) {
        return Err(AssetError::InvalidProject(
            "SC allocation map does not contain every engine entry 0..88".to_owned(),
        ));
    }
    Ok(entries)
}

fn stock_sc_allocation_map() -> [ScExtractEntry; SC_ENTRY_COUNT] {
    let mut entries = [ScExtractEntry::default(); SC_ENTRY_COUNT];
    for (entry_id, entry) in entries.iter_mut().enumerate() {
        entry.allocation_size = engine_allocations::allocation_size("sc.cpk", entry_id as u32)
            .expect("stock SC allocation table contains every entry");
        entry.metadata = engine_allocations::sc_metadata(entry_id as u32);
    }
    entries
}

fn index_dialogue_entries_by_entry_id(
    entries: BTreeMap<u32, codec::script::ScenarioDialogueEntry>,
) -> Result<BTreeMap<u32, codec::script::ScenarioDialogueEntry>, AssetError> {
    validate_entry_id_map(
        &entries,
        "scenario-dialogue.json",
        |entry| entry.entry_id,
    )?;
    Ok(entries)
}

fn index_state_entries_by_entry_id(
    entries: BTreeMap<u32, codec::script::ScenarioStateEntry>,
) -> Result<BTreeMap<u32, codec::script::ScenarioStateEntry>, AssetError> {
    validate_entry_id_map(
        &entries,
        ".rz-internal/sc-build-state.json.gz",
        |entry| entry.entry_id,
    )?;
    Ok(entries)
}

fn validate_entry_id_map<T>(
    entries: &BTreeMap<u32, T>,
    label: &str,
    entry_id_of: impl Fn(&T) -> u32,
) -> Result<(), AssetError> {
    if entries.len() != SC_ENTRY_COUNT {
        return Err(AssetError::InvalidProject(format!(
            "{label} contains {} entries, expected {SC_ENTRY_COUNT}",
            entries.len()
        )));
    }
    for expected in 0..SC_ENTRY_COUNT {
        let expected_id = expected as u32;
        let entry = entries.get(&expected_id).ok_or_else(|| {
            AssetError::InvalidProject(format!(
                "{label} omits engine entry_id {expected_id}"
            ))
        })?;
        let actual_id = entry_id_of(entry);
        if actual_id != expected_id {
            return Err(AssetError::InvalidProject(format!(
                "{label} map key {expected_id} contains mismatched entry_id {actual_id}"
            )));
        }
    }
    Ok(())
}

pub fn extract_project(
    input: &Path,
    output_directory: &Path,
    options: ExtractOptions,
) -> Result<ExtractReport, AssetError> {
    if output_directory.exists() {
        return Err(AssetError::OutputExists(output_directory.to_owned()));
    }
    let stage = staging_directory(output_directory);
    if stage.exists() {
        fs::remove_dir_all(&stage)?;
    }
    fs::create_dir_all(&stage)?;

    let result = extract_to_stage(input, &stage, options);
    match result {
        Ok(report) => match fs::rename(&stage, output_directory) {
            Ok(()) => Ok(report),
            Err(error) => {
                let _ = fs::remove_dir_all(&stage);
                Err(AssetError::Io(error))
            }
        },
        Err(error) => {
            let _ = fs::remove_dir_all(&stage);
            Err(error)
        }
    }
}

fn extract_to_stage(
    input: &Path,
    stage: &Path,
    options: ExtractOptions,
) -> Result<ExtractReport, AssetError> {
    let source_name = input
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("source.bin")
        .to_owned();
    let lower = source_name.to_ascii_lowercase();

    if options.raw_only && (lower == "lt.bin" || lower == "pr.bin") {
        let relative = "source.bin";
        fs::copy(input, stage.join(relative))?;
        write_manifest(
            stage,
            &ProjectManifest {
                schema_version: PROJECT_SCHEMA_VERSION,
                mode: ProjectMode::RawOnly,
                source_name,
                source: ProjectSource::RawFile {
                    path: relative.to_owned(),
                },
            },
        )?;
        return Ok(ExtractReport {
            files: 1,
            editable: false,
        });
    }

    if lower == "lt.bin" {
        if options.charset_map.is_some() || options.debug_script_ir || options.use_stock_charset || options.use_stock_allocation {
            return Err(AssetError::InvalidProject(
                "charset/script/--use-stock-allocation extraction options are not valid for lt.bin".to_owned(),
            ));
        }
        let bytes = fs::read(input)?;
        let document = if let Some(path) = options.allocation_map.as_ref() {
            let allocation = load_lt_allocation_map(path)?;
            if bytes.len() != allocation.allocation_size {
                return Err(AssetError::InvalidProject(format!(
                    "lt.bin is {:#x} bytes, but LT allocation map requires {:#x} bytes",
                    bytes.len(), allocation.allocation_size
                )));
            }
            codec::lt_font::decode_with_glyph_count(&bytes, stage, allocation.glyph_count)?
        } else {
            codec::lt_font::decode(&bytes, stage)?
        };
        write_manifest(
            stage,
            &ProjectManifest {
                schema_version: PROJECT_SCHEMA_VERSION,
                mode: ProjectMode::Editable,
                source_name,
                source: ProjectSource::LtFont { document },
            },
        )?;
        return Ok(ExtractReport {
            files: 1,
            editable: true,
        });
    }

    if lower == "pr.bin" {
        return Err(AssetError::UnsupportedAsset {
            entry: source_name,
            reason: "pr.bin is a mixed fixed-slot bank: the engine uses package slots, direct descriptor/palette slots, and separate compressed-atlas slots; editable rebuild is disabled until all three encoders are proven. Use --raw-only only for forensic extraction".to_owned(),
        });
    }

    if lower.ends_with(".cpk") {
        let file = File::open(input)?;
        let reader = CpkReader::<_, DummyDecryptor>::new_with_encryption(file)
            .map_err(AssetError::Archive)?;
        return extract_cpk(reader, input, stage, options);
    }

    Err(AssetError::OutOfScopeResource(source_name))
}

const STOCK_SC_CPK_SHA256: &str = "902d29e71465e93227fa098d4a5e54507a8d8d1a31334e68a2727368c122a563";

fn enforce_sc_extract_metadata_policy(
    input_cpk: &Path,
    options: &ExtractOptions,
) -> Result<(), AssetError> {
    let digest = sha256_file_hex(input_cpk)?;
    if digest == STOCK_SC_CPK_SHA256 {
        return Ok(());
    }

    let missing_charset = options.charset_map.is_none() && !options.use_stock_charset;
    let missing_allocation = options.allocation_map.is_none() && !options.use_stock_allocation;
    if missing_charset || missing_allocation {
        let mut requirements = Vec::new();
        if missing_charset {
            requirements.push("--charset-map <font.tbl|charset.json> or --use-stock-charset");
        }
        if missing_allocation {
            requirements.push("--allocation-map <sc-allocation.json> or --use-stock-allocation");
        }
        return Err(AssetError::InvalidProject(format!(
            "sc.cpk hash {digest} is not a known stock archive; pass {}",
            requirements.join(" and ")
        )));
    }
    Ok(())
}

fn sha256_file_hex(path: &Path) -> Result<String, AssetError> {
    let bytes = fs::read(path)?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(hex::encode(hasher.finalize()))
}

fn extract_cpk<R>(
    mut reader: CpkReader<R, DummyDecryptor>,
    input_cpk: &Path,
    stage: &Path,
    options: ExtractOptions,
) -> Result<ExtractReport, AssetError>
where
    R: Read + Seek,
{
    let files = reader.get_files().map_err(AssetError::Archive)?;
    let metadata = *reader.metadata().ok_or_else(|| {
        AssetError::InvalidProject("CPK metadata was not initialized".to_owned())
    })?;
    let archive_name = input_cpk
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("archive.cpk")
        .to_owned();
    let mode = if options.raw_only {
        ProjectMode::RawOnly
    } else {
        ProjectMode::Editable
    };
    let is_sc = archive_name.eq_ignore_ascii_case("sc.cpk");
    if is_sc && !options.raw_only {
        enforce_sc_extract_metadata_policy(input_cpk, &options)?;
    }
    if options.debug_script_ir && (!is_sc || options.raw_only) {
        return Err(AssetError::InvalidProject(
            "--debug-script-ir is available only for editable sc.cpk extraction".to_owned(),
        ));
    }
    let sc_charset_map = if is_sc && !options.raw_only {
        Some(match options.charset_map.as_ref() {
            Some(path) => codec::charset::load_map(path)?,
            None => codec::charset::default_map().clone(),
        })
    } else {
        None
    };
    let sc_allocation_map = if is_sc && !options.raw_only {
        Some(match options.allocation_map.as_ref() {
            Some(path) => load_sc_allocation_map(path)?,
            None => stock_sc_allocation_map(),
        })
    } else {
        None
    };
    let portable_dialogue_metadata = if is_sc && !options.raw_only {
        let metadata = codec::script::load_portable_dialogue_metadata(input_cpk)?;
        if let Some(document) = metadata.as_ref() {
            if let Some(expected) = document.archive_sha256.as_deref() {
                let actual = sha256_file_hex(input_cpk)?;
                if !actual.eq_ignore_ascii_case(expected) {
                    return Err(AssetError::InvalidProject(format!(
                        "portable dialogue metadata companion targets SC hash {expected}, but input archive hash is {actual}"
                    )));
                }
            }
        }
        metadata
    } else {
        None
    };
    let mut sc_ids = BTreeSet::new();
    let mut assets = Vec::with_capacity(files.len());
    let mut processing_order = (0..files.len()).collect::<Vec<_>>();
    if is_sc {
        // Deterministic project presentation follows engine-visible IDs. The
        // original CPK position is still stored in AssetEntry::order and is
        // restored by the build pipeline before archive emission.
        processing_order.sort_by_key(|index| files[*index].id().unwrap_or(u32::MAX));
    }

    for index in processing_order {
        let file = &files[index];
        let extracted = reader.extract_file(file).map_err(AssetError::Archive)?;
        let order = u32::try_from(index)
            .map_err(|_| AssetError::InvalidProject("too many CPK entries".to_owned()))?;
        if is_sc {
            let entry_id = file.id().ok_or_else(|| {
                AssetError::InvalidProject(format!(
                    "sc.cpk entry at archive order {order} has no ITOC engine ID"
                ))
            })?;
            let index = usize::try_from(entry_id).map_err(|_| {
                AssetError::InvalidProject("SC entry ID exceeds usize".to_owned())
            })?;
            if index >= SC_ENTRY_COUNT || !sc_ids.insert(entry_id) {
                return Err(AssetError::InvalidProject(format!(
                    "sc.cpk has invalid or duplicate engine ID {entry_id}"
                )));
            }
        }
        let context = DecodeContext {
            archive_name: &archive_name,
            directory: file.directory(),
            file_name: file.file_name(),
            order,
            id: file.id(),
        };
        codec::ensure_in_scope(&context, &extracted)?;
        let kind = if options.raw_only {
            let safe_name = safe_component(file.file_name());
            let relative = format!("{order:05}-{safe_name}");
            let target = stage.join(&relative);
            fs::write(target, extracted)?;
            AssetKind::Raw { path: relative }
        } else {
            // Image assets use stable root-level stems. SC decode first writes
            // per-entry machine state into a staging-only internal directory;
            // write_routing_document later consolidates it into one compressed
            // bundle and removes those temporary files. Engine-visible ITOC ID
            // remains distinct from archive order throughout.
            let output_stem = if is_sc {
                format!("{:05}", file.id().expect("SC ID was validated"))
            } else {
                format!("{order:05}")
            };
            if is_sc {
                let entry_id = file.id().expect("SC ID was validated");
                let sc_entry = sc_allocation_map
                    .as_ref()
                    .expect("SC allocation map was initialized")
                    [usize::try_from(entry_id).expect("SC ID fits usize")];
                codec::decode_editable_with_options(
                    &extracted,
                    &context,
                    stage,
                    &output_stem,
                    codec::EditableDecodeOptions {
                        allocation_size: Some(sc_entry.allocation_size),
                        charset_map: sc_charset_map.as_ref(),
                        engine_metadata: sc_entry.metadata,
                    },
                )?
            } else {
                codec::decode_editable(&extracted, &context, stage, &output_stem)?
            }
        };
        assets.push(AssetEntry {
            order,
            directory: file.directory().to_owned(),
            file_name: file.file_name().to_owned(),
            id: file.id(),
            user_string: file.user_string().to_owned(),
            kind,
        });
    }

    if is_sc {
        if sc_ids.len() != SC_ENTRY_COUNT
            || (0..SC_ENTRY_COUNT).any(|entry_id| !sc_ids.contains(&(entry_id as u32)))
        {
            return Err(AssetError::InvalidProject(
                "sc.cpk does not contain every engine ID 0..88".to_owned(),
            ));
        }
        if !options.raw_only {
            // Do not mirror extraction input files into the SC project.
            // `--charset-map` and `--allocation-map` are caller-supplied inputs;
            // copying them into the extract directory creates duplicate, stale
            // project data (`charset.json`, `charset-map.source`,
            // `sc-allocation.json`). The compact machine state already records
            // the runtime allocation/metadata contract needed for rebuild, and
            // the charset remains an explicit build input when a non-stock map is
            // required.
            let presentation_order = codec::script::write_routing_document(
                stage,
                &assets,
                options.debug_script_ir,
                sc_charset_map.as_ref().expect("SC charset was initialized"),
                portable_dialogue_metadata.as_ref(),
            )?;
            codec::script::apply_presentation_order(&mut assets, &presentation_order)?;
        }
    }

    let count = u32::try_from(files.len())
        .map_err(|_| AssetError::InvalidProject("too many CPK entries".to_owned()))?;
    write_manifest(
        stage,
        &ProjectManifest {
            schema_version: PROJECT_SCHEMA_VERSION,
            mode: mode.clone(),
            source_name: archive_name,
            source: ProjectSource::Cpk {
                cpk: cpk_project(metadata),
                assets,
            },
        },
    )?;
    Ok(ExtractReport {
        files: count,
        editable: mode == ProjectMode::Editable,
    })
}


#[derive(Debug, Deserialize)]
struct FixedWidthJsonDocument {
    glyphs: Vec<FixedWidthJsonGlyph>,
}

#[derive(Debug, Deserialize)]
struct FixedWidthJsonGlyph {
    #[serde(rename = "char")]
    glyph: String,
    advance: u32,
}

#[derive(Debug, Clone)]
struct FixedWidthPolicy {
    explicit: HashMap<char, u32>,
    default_advance: u32,
}

fn validate_width_advance(value: u32, context: &str) -> Result<u32, AssetError> {
    if !(1..=24).contains(&value) {
        return Err(AssetError::InvalidProject(format!(
            "{context} has invalid advance {value}; expected 1..24"
        )));
    }
    Ok(value)
}


fn strip_font_config_inline_comment(value: &str) -> &str {
    // In font.cnf, '#' can be a real glyph in [groups] values, e.g.
    // WIDE=#$%&@mw. Treat '#' as a comment only at line start or when it is
    // separated from the value by whitespace.
    for (index, ch) in value.char_indices() {
        if ch == '#' && (index == 0 || value[..index].chars().last().map_or(false, char::is_whitespace)) {
            return &value[..index];
        }
    }
    value
}

fn strip_font_config_group_comment(value: &str) -> &str {
    // Group values are character sets; a leading '#' is the literal number-sign
    // glyph. Only a whitespace-separated '#' starts a trailing comment.
    for (index, ch) in value.char_indices() {
        if ch == '#' && index > 0 && value[..index].chars().last().map_or(false, char::is_whitespace) {
            return value[..index].trim_end();
        }
    }
    value.trim_end()
}

fn load_fixed_width_table(path: &Path) -> Result<FixedWidthPolicy, AssetError> {
    let bytes = fs::read(path)?;
    if path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.eq_ignore_ascii_case("json"))
        .unwrap_or(false)
    {
        return load_fixed_width_json(&bytes);
    }
    load_font_width_config(&String::from_utf8(bytes).map_err(|_| {
        AssetError::InvalidProject(format!(
            "font width config {} is not valid UTF-8",
            path.display()
        ))
    })?)
}

fn load_fixed_width_json(bytes: &[u8]) -> Result<FixedWidthPolicy, AssetError> {
    let document: FixedWidthJsonDocument = serde_json::from_slice(bytes)?;
    let mut explicit = HashMap::with_capacity(document.glyphs.len());
    for glyph in document.glyphs {
        let mut chars = glyph.glyph.chars();
        let ch = chars.next().ok_or_else(|| {
            AssetError::InvalidProject("width JSON glyph has an empty char field".to_owned())
        })?;
        if chars.next().is_some() {
            return Err(AssetError::InvalidProject(format!(
                "width JSON glyph {:?} is not a single Unicode scalar",
                glyph.glyph
            )));
        }
        let advance = validate_width_advance(glyph.advance, "width JSON glyph")?;
        explicit.insert(ch, advance);
    }
    Ok(FixedWidthPolicy {
        explicit,
        // Compiled patcher JSON is already per-glyph; keep the historical
        // fallback for unmapped/stock glyphs.
        default_advance: 24,
    })
}

fn load_font_width_config(text: &str) -> Result<FixedWidthPolicy, AssetError> {
    let mut section = String::new();
    let mut buckets = BTreeMap::<String, u32>::new();
    let mut groups = Vec::<(String, String)>::new();
    let mut advance_groups = Vec::<(u32, String)>::new();
    let mut char_widths = Vec::<(String, u32)>::new();
    let mut default_bucket = None::<String>;
    let mut default_advance = None::<u32>;

    for (line_index, raw_line) in text.lines().enumerate() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let structural_line = strip_font_config_inline_comment(trimmed).trim();
        if structural_line.starts_with('[') && structural_line.ends_with(']') {
            section = structural_line[1..structural_line.len() - 1].trim().to_ascii_lowercase();
            continue;
        }
        let Some((key, raw_value)) = trimmed.split_once('=') else {
            return Err(AssetError::InvalidProject(format!(
                "font width config line {} is not key=value",
                line_index + 1
            )));
        };
        let key = key.trim().to_owned();
        let value = match section.as_str() {
            "groups" => strip_font_config_group_comment(raw_value.trim()).to_owned(),
            _ => strip_font_config_inline_comment(raw_value.trim()).trim().to_owned(),
        };
        match section.as_str() {
            "buckets" => {
                let advance = value.parse::<u32>().map_err(|_| {
                    AssetError::InvalidProject(format!(
                        "font width config line {} has non-integer bucket width",
                        line_index + 1
                    ))
                })?;
                buckets.insert(key, validate_width_advance(advance, "width bucket")?);
            }
            "groups" => groups.push((key, value)),
            "advance_groups" | "advance-groups" | "width_groups" | "width-groups" => {
                let advance = key.parse::<u32>().map_err(|_| {
                    AssetError::InvalidProject(format!(
                        "font width config line {} has non-integer advance-group key",
                        line_index + 1
                    ))
                })?;
                advance_groups.push((validate_width_advance(advance, "advance group")?, value));
            }
            "chars" | "widths" => {
                let advance = value.parse::<u32>().map_err(|_| {
                    AssetError::InvalidProject(format!(
                        "font width config line {} has non-integer character width",
                        line_index + 1
                    ))
                })?;
                char_widths.push((key, validate_width_advance(advance, "character width")?));
            }
            "default" if key == "bucket" => default_bucket = Some(value),
            "default" if key == "advance" => {
                let advance = value.parse::<u32>().map_err(|_| {
                    AssetError::InvalidProject(format!(
                        "font width config line {} has non-integer default advance",
                        line_index + 1
                    ))
                })?;
                default_advance = Some(validate_width_advance(advance, "default advance")?);
            }
            "default" => {
                return Err(AssetError::InvalidProject(format!(
                    "font width config line {} has unsupported [default] key {key:?}",
                    line_index + 1
                )))
            }
            "" => {
                return Err(AssetError::InvalidProject(format!(
                    "font width config line {} appears before any section",
                    line_index + 1
                )))
            }
            other => {
                return Err(AssetError::InvalidProject(format!(
                    "font width config line {} is in unsupported section [{other}]",
                    line_index + 1
                )))
            }
        }
    }

    let default_advance = match (default_advance, default_bucket) {
        (Some(advance), None) => advance,
        (None, Some(bucket)) => *buckets.get(&bucket).ok_or_else(|| {
            AssetError::InvalidProject(format!(
                "font width config default bucket {bucket:?} is not defined in [buckets]"
            ))
        })?,
        (Some(_), Some(_)) => {
            return Err(AssetError::InvalidProject(
                "font width config [default] may use either advance=... or bucket=..., not both".to_owned(),
            ))
        }
        (None, None) if !char_widths.is_empty() => 24,
        (None, None) => {
            return Err(AssetError::InvalidProject(
                "font width config is missing [default] bucket=... or advance=...".to_owned(),
            ))
        }
    };

    let mut explicit = HashMap::<char, u32>::new();
    for (bucket, spec) in groups {
        let advance = *buckets.get(&bucket).ok_or_else(|| {
            AssetError::InvalidProject(format!(
                "font width config group {bucket:?} references an undefined bucket"
            ))
        })?;
        for ch in expand_width_group_spec(&spec) {
            if let Some(previous) = explicit.insert(ch, advance) {
                if previous != advance {
                    return Err(AssetError::InvalidProject(format!(
                        "font width config maps character {ch:?} to both {previous}px and {advance}px"
                    )));
                }
            }
        }
    }
    for (advance, spec) in advance_groups {
        for ch in expand_width_group_spec(&spec) {
            if let Some(previous) = explicit.insert(ch, advance) {
                if previous != advance {
                    return Err(AssetError::InvalidProject(format!(
                        "font width config maps character {ch:?} to both {previous}px and {advance}px"
                    )));
                }
            }
        }
    }
    for (key, advance) in char_widths {
        let ch = parse_width_char_key(&key)?;
        if let Some(previous) = explicit.insert(ch, advance) {
            if previous != advance {
                return Err(AssetError::InvalidProject(format!(
                    "font width config maps character {ch:?} to both {previous}px and {advance}px"
                )));
            }
        }
    }

    Ok(FixedWidthPolicy {
        explicit,
        default_advance,
    })
}

fn parse_width_char_key(key: &str) -> Result<char, AssetError> {
    if key == "<space>" {
        return Ok(' ');
    }
    if let Some(hex) = key.strip_prefix("U+").or_else(|| key.strip_prefix("u+")) {
        let value = u32::from_str_radix(hex, 16).map_err(|_| {
            AssetError::InvalidProject(format!("invalid width table Unicode key {key:?}"))
        })?;
        return char::from_u32(value).ok_or_else(|| {
            AssetError::InvalidProject(format!("width table Unicode key {key:?} is not a scalar value"))
        });
    }
    let mut chars = key.chars();
    let ch = chars.next().ok_or_else(|| {
        AssetError::InvalidProject("empty width table character key".to_owned())
    })?;
    if chars.next().is_some() {
        return Err(AssetError::InvalidProject(format!(
            "width table character key {key:?} is not a single Unicode scalar; use U+XXXX"
        )));
    }
    Ok(ch)
}

fn expand_width_group_spec(spec: &str) -> Vec<char> {
    let mut out = Vec::<char>::new();
    let mut chars = spec.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '<' {
            let mut token = String::new();
            while let Some(next) = chars.next() {
                if next == '>' {
                    break;
                }
                token.push(next);
            }
            match token.as_str() {
                "space" => out.push(' '),
                "upper" => out.extend('A'..='Z'),
                "lower" => out.extend('a'..='z'),
                "ascii-upper" => out.extend('A'..='Z'),
                "ascii-lower" => out.extend('a'..='z'),
                "digits" => out.extend('0'..='9'),
                "ascii-printable" => out.extend((0x20u8..=0x7eu8).map(char::from)),
                "vietnamese" => out.extend(VIETNAMESE_WIDTH_SELECTOR.chars()),
                "viet-upper" => out.extend(VIETNAMESE_UPPER_WIDTH_SELECTOR.chars()),
                "viet-lower" => out.extend(VIETNAMESE_LOWER_WIDTH_SELECTOR.chars()),
                "" => {
                    out.push('<');
                    out.push('>');
                }
                _ => {
                    out.push('<');
                    out.extend(token.chars());
                    out.push('>');
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

const VIETNAMESE_LOWER_WIDTH_SELECTOR: &str =
    "àáảãạằắẳẵặầấẩẫậèéẻẽẹềếểễệìíỉĩịòóỏõọồốổỗộờớởỡợùúủũụừứửữựỳýỷỹỵăâêôơưđ";
const VIETNAMESE_UPPER_WIDTH_SELECTOR: &str =
    "ÀÁẢÃẠẰẮẲẴẶẦẤẨẪẬÈÉẺẼẸỀẾỂỄỆÌÍỈĨỊÒÓỎÕỌỒỐỔỖỘỜỚỞỠỢÙÚỦŨỤỪỨỬỮỰỲÝỶỸỴĂÂÊÔƠƯĐ";
const VIETNAMESE_WIDTH_SELECTOR: &str =
    "àáảãạằắẳẵặầấẩẫậèéẻẽẹềếểễệìíỉĩịòóỏõọồốổỗộờớởỡợùúủũụừứửữựỳýỷỹỵăâêôơưđÀÁẢÃẠẰẮẲẴẶẦẤẨẪẬÈÉẺẼẸỀẾỂỄỆÌÍỈĨỊÒÓỎÕỌỒỐỔỖỘỜỚỞỠỢÙÚỦŨỤỪỨỬỮỰỲÝỶỸỴĂÂÊÔƠƯĐ";

fn apply_fixed_width_wrap(
    entries: &mut BTreeMap<u32, codec::script::ScenarioDialogueEntry>,
    widths: &FixedWidthPolicy,
    row_limit: u32,
    mode: WrapMode,
    rows: u32,
) -> Result<(), AssetError> {
    if row_limit == 0 {
        return Err(AssetError::InvalidProject(
            "--wrap-width-px must be greater than zero".to_owned(),
        ));
    }
    if rows == 0 {
        return Err(AssetError::InvalidProject(
            "--wrap-rows must be greater than zero".to_owned(),
        ));
    }
    let rows_per_screen = usize::try_from(rows).map_err(|_| {
        AssetError::InvalidProject("--wrap-rows exceeds this platform's usize".to_owned())
    })?;

    for entry in entries.values_mut() {
        let entry_id = entry.entry_id;
        for dialogue in &mut entry.dialogues {
            let source_lines = dialogue.engine_lines();
            let should_wrap = source_lines
                .iter()
                .any(|line| line.chars().any(|ch| widths.explicit.contains_key(&ch)));
            if !should_wrap {
                continue;
            }
            let mut materialized_rows = Vec::<String>::new();
            let mut row_joiners = Vec::<String>::new();
            for paragraph in source_lines {
                let (mut rows_for_paragraph, mut joiners_for_paragraph) = match mode {
                    WrapMode::Word => {
                        let wrapped =
                            wrap_text_to_engine_rows_word(&paragraph, widths, row_limit)?;
                        (wrapped.rows, wrapped.row_joiners)
                    }
                    WrapMode::Legacy => {
                        let rows =
                            wrap_text_to_engine_rows_legacy(&paragraph, widths, row_limit)?;
                        let joiners = rows
                            .iter()
                            .skip(1)
                            .map(|_| String::new())
                            .collect::<Vec<_>>();
                        (rows, joiners)
                    }
                };
                if !materialized_rows.is_empty() && !rows_for_paragraph.is_empty() {
                    row_joiners.push(String::new());
                }
                materialized_rows.append(&mut rows_for_paragraph);
                row_joiners.append(&mut joiners_for_paragraph);
            }
            if row_joiners.len() != materialized_rows.len().saturating_sub(1) {
                return Err(AssetError::InvalidProject(format!(
                    "internal wrap metadata mismatch for entry {} marker {}: {} rows but {} joiners",
                    entry_id,
                    dialogue.marker_index,
                    materialized_rows.len(),
                    row_joiners.len()
                )));
            }
            let screens = materialized_rows
                .chunks(rows_per_screen)
                .map(|chunk| chunk.to_vec())
                .collect::<Vec<_>>();
            dialogue.set_engine_screens_for_build_with_joiners(screens, row_joiners);
        }
    }
    Ok(())
}

fn glyph_advance(ch: char, widths: &FixedWidthPolicy) -> u32 {
    widths.explicit.get(&ch).copied().unwrap_or(widths.default_advance)
}

fn measure_width(text: &str, widths: &FixedWidthPolicy) -> u32 {
    text.chars().map(|ch| glyph_advance(ch, widths)).sum()
}

fn validate_materialized_rows(
    rows: &[String],
    widths: &FixedWidthPolicy,
    row_limit: u32,
    mode_name: &str,
) -> Result<(), AssetError> {
    for row in rows {
        let width = measure_width(row, widths);
        if width > row_limit {
            return Err(AssetError::InvalidProject(format!(
                "{mode_name} fixed-wrap row {row:?} is {width}px wide, exceeding row wrap limit {row_limit}px"
            )));
        }
    }
    Ok(())
}

fn wrap_text_to_engine_rows_legacy(
    text: &str,
    widths: &FixedWidthPolicy,
    row_limit: u32,
) -> Result<Vec<String>, AssetError> {
    let mut rows = Vec::<String>::new();
    let mut current = String::new();
    let mut current_width = 0u32;
    for ch in text.chars() {
        let advance = glyph_advance(ch, widths);
        if advance > row_limit {
            return Err(AssetError::InvalidProject(format!(
                "character {ch:?} has advance {advance}px, exceeding row wrap limit {row_limit}px"
            )));
        }
        if !current.is_empty() && current_width + advance > row_limit {
            rows.push(std::mem::take(&mut current));
            current_width = 0;
        }
        current.push(ch);
        current_width += advance;
    }
    if !current.is_empty() || rows.is_empty() {
        rows.push(current);
    }
    validate_materialized_rows(&rows, widths, row_limit, "legacy")?;
    Ok(rows)
}

struct WrappedRows {
    rows: Vec<String>,
    row_joiners: Vec<String>,
}

fn push_word_for_materialized_rows(
    rows: &mut Vec<String>,
    row_joiners: &mut Vec<String>,
    current: &mut String,
    current_width: &mut u32,
    word: &str,
    word_width: u32,
    widths: &FixedWidthPolicy,
    row_limit: u32,
) {
    if current.is_empty() {
        current.push_str(word);
        *current_width = word_width;
        return;
    }
    let with_space = *current_width + glyph_advance(' ', widths) + word_width;
    if with_space <= row_limit {
        current.push(' ');
        current.push_str(word);
        *current_width = with_space;
    } else {
        rows.push(std::mem::take(current));
        row_joiners.push(" ".to_owned());
        current.push_str(word);
        *current_width = word_width;
    }
}

fn wrap_text_to_engine_rows_word(
    text: &str,
    widths: &FixedWidthPolicy,
    row_limit: u32,
) -> Result<WrappedRows, AssetError> {
    let mut rows = Vec::<String>::new();
    let mut row_joiners = Vec::<String>::new();
    let mut current = String::new();
    let mut current_width = 0u32;

    for word in text.split_whitespace() {
        let word_width = measure_width(word, widths);
        if word_width <= row_limit {
            push_word_for_materialized_rows(
                &mut rows,
                &mut row_joiners,
                &mut current,
                &mut current_width,
                word,
                word_width,
                widths,
                row_limit,
            );
            continue;
        }

        if !current.is_empty() {
            rows.push(std::mem::take(&mut current));
            row_joiners.push(" ".to_owned());
            current_width = 0;
        }

        let mut forced = String::new();
        let mut forced_width = 0u32;
        for ch in word.chars() {
            let advance = glyph_advance(ch, widths);
            if advance > row_limit {
                return Err(AssetError::InvalidProject(format!(
                    "character {ch:?} has advance {advance}px, exceeding row wrap limit {row_limit}px"
                )));
            }
            if !forced.is_empty() && forced_width + advance > row_limit {
                rows.push(std::mem::take(&mut forced));
                row_joiners.push(String::new());
                forced_width = 0;
            }
            forced.push(ch);
            forced_width += advance;
        }
        if !forced.is_empty() {
            current = forced;
            current_width = forced_width;
        }
    }

    if !current.is_empty() || rows.is_empty() {
        rows.push(current);
    }
    validate_materialized_rows(&rows, widths, row_limit, "word")?;
    if row_joiners.len() != rows.len().saturating_sub(1) {
        return Err(AssetError::InvalidProject(
            "internal word-wrap row joiner count mismatch".to_owned(),
        ));
    }
    Ok(WrappedRows { rows, row_joiners })
}

pub fn build_project(
    project_directory: &Path,
    output: &Path,
    options: BuildOptions,
) -> Result<BuildReport, AssetError> {
    if output.exists() {
        return Err(AssetError::OutputExists(output.to_owned()));
    }
    match (&options.eboot_in, &options.eboot_out) {
        (Some(_), None) | (None, Some(_)) => {
            return Err(AssetError::InvalidProject(
                "--eboot-in and --eboot-out must be provided together".to_owned(),
            ));
        }
        (_, Some(path)) if path.exists() => return Err(AssetError::OutputExists(path.clone())),
        _ => {}
    }

    if options.wrap_width_px.is_some() && options.wrap_width_table.is_none() {
        return Err(AssetError::InvalidProject(
            "--wrap-width-px requires --wrap-width-table <font.cnf>".to_owned(),
        ));
    }
    if options.wrap_mode.is_some() && options.wrap_width_table.is_none() {
        return Err(AssetError::InvalidProject(
            "--wrap-mode requires --wrap-width-table <font.cnf>".to_owned(),
        ));
    }
    if options.wrap_rows.is_some() && options.wrap_width_table.is_none() {
        return Err(AssetError::InvalidProject(
            "--wrap-rows requires --wrap-width-table <font.cnf>".to_owned(),
        ));
    }
    let manifest_path = project_directory.join(PROJECT_FILE_NAME);
    let manifest: ProjectManifest = serde_json::from_slice(&fs::read(&manifest_path)?)?;
    validate_manifest(&manifest)?;
    let is_editable_sc_project = matches!(&manifest.mode, ProjectMode::Editable)
        && manifest.source_name.eq_ignore_ascii_case("sc.cpk")
        && matches!(&manifest.source, ProjectSource::Cpk { .. });
    let portable_metadata_output = is_editable_sc_project
        .then(|| codec::script::dialogue_metadata_companion_path(output));
    if let Some(path) = portable_metadata_output.as_ref() {
        if path.exists() {
            return Err(AssetError::OutputExists(path.clone()));
        }
    }

    let stage = build_staging_directory(output);
    let staged_output = build_staging_file(output);
    if stage.exists() {
        fs::remove_dir_all(&stage)?;
    }
    if staged_output.exists() {
        fs::remove_file(&staged_output)?;
    }
    fs::create_dir_all(&stage)?;

    let result = (|| -> Result<BuildOutcome, AssetError> {
        match &manifest.source {
            ProjectSource::Cpk { cpk, assets } => build_cpk(
                project_directory,
                &staged_output,
                &stage,
                &manifest,
                cpk,
                assets,
                &options,
            ),
            ProjectSource::LtFont { document } => {
                if options.charset_map.is_some()
                    || options.wrap_width_px.is_some()
                    || options.wrap_width_table.is_some()
                    || options.wrap_rows.is_some()
                    || options.wrap_mode.is_some()
                {
                    return Err(AssetError::InvalidProject(
                        "--charset-map/wrap options are valid only when building sc.cpk".to_owned(),
                    ));
                }
                if options.eboot_in.is_none() || options.eboot_out.is_none() {
                    return Err(AssetError::InvalidProject(
                        "building editable lt.bin requires --eboot-in <eboot.bin.elf> and --eboot-out <patched.bin.elf> so rz-tool can update LT allocation".to_owned(),
                    ));
                }
                let document_path = project_directory.join(document);
                let bytes = codec::lt_font::encode(&document_path)?;
                let glyph_count = codec::lt_font::document_glyph_count(&document_path)?;
                fs::write(&staged_output, &bytes)?;
                Ok(BuildOutcome {
                    report: BuildReport {
                        files: 1,
                        output_size: u64::try_from(bytes.len()).unwrap(),
                        eboot_patch: None,
                        lt_eboot_patch: None,
                    },
                    sc_patch_plan: None,
                    lt_patch_plan: Some(LtPatchPlan {
                        glyph_count,
                        allocation_size: bytes.len(),
                    }),
                    portable_dialogue_metadata: None,
                })
            }
            ProjectSource::RawFile { path } => {
                if options.eboot_in.is_some()
                    || options.force
                    || options.charset_map.is_some()
                    || options.wrap_width_px.is_some()
                    || options.wrap_width_table.is_some()
                    || options.wrap_rows.is_some()
                    || options.wrap_mode.is_some()
                {
                    return Err(AssetError::InvalidProject(
                        "--eboot-in/--eboot-out/-f/--charset-map/wrap options are valid only when building editable sc.cpk or lt.bin".to_owned(),
                    ));
                }
                let bytes = fs::read(project_directory.join(path))?;
                fs::write(&staged_output, &bytes)?;
                Ok(BuildOutcome {
                    report: BuildReport {
                        files: 1,
                        output_size: u64::try_from(bytes.len()).unwrap(),
                        eboot_patch: None,
                        lt_eboot_patch: None,
                    },
                    sc_patch_plan: None,
                    lt_patch_plan: None,
                    portable_dialogue_metadata: None,
                })
            }
        }
    })();

    let _ = fs::remove_dir_all(&stage);
    let mut staged_eboot = None::<PathBuf>;
    let mut staged_portable_metadata = None::<PathBuf>;
    let final_result = match result {
        Ok(mut outcome) => {
            if let Some(plan) = outcome.sc_patch_plan.as_ref() {
                if let (Some(input), Some(output_eboot)) =
                    (options.eboot_in.as_ref(), options.eboot_out.as_ref())
                {
                    let stage_eboot = build_staging_file(output_eboot);
                    if stage_eboot.exists() {
                        fs::remove_file(&stage_eboot)?;
                    }
                    let require_vwf_runtime = options.charset_map.is_some()
                        || options.wrap_width_table.is_some();
                    let patch_report = match eboot::patch_sc_elf(
                        input,
                        &stage_eboot,
                        plan,
                        require_vwf_runtime,
                        options.force,
                    ) {
                        Ok(report) => report,
                        Err(error) => {
                            let _ = fs::remove_file(&staged_output);
                            let _ = fs::remove_file(&stage_eboot);
                            return Err(error);
                        }
                    };
                    outcome.report.eboot_patch = Some(patch_report);
                    staged_eboot = Some(stage_eboot);
                } else if plan.differs_from_stock() {
                    let _ = fs::remove_file(&staged_output);
                    return Err(AssetError::InvalidProject(
                        "rebuilt sc.cpk changes sector capacity or executable-resident script counts; provide --eboot-in <eboot.bin.elf> and --eboot-out <patched.bin.elf>"
                            .to_owned(),
                    ));
                }
            } else if let Some(plan) = outcome.lt_patch_plan {
                let (input, output_eboot) = match (options.eboot_in.as_ref(), options.eboot_out.as_ref()) {
                    (Some(input), Some(output_eboot)) => (input, output_eboot),
                    _ => {
                        let _ = fs::remove_file(&staged_output);
                        return Err(AssetError::InvalidProject(
                            "building editable lt.bin requires --eboot-in and --eboot-out".to_owned(),
                        ));
                    }
                };
                let stage_eboot = build_staging_file(output_eboot);
                if stage_eboot.exists() {
                    fs::remove_file(&stage_eboot)?;
                }
                let patch_report = match eboot::patch_lt_elf(input, &stage_eboot, plan, options.force) {
                    Ok(report) => report,
                    Err(error) => {
                        let _ = fs::remove_file(&staged_output);
                        let _ = fs::remove_file(&stage_eboot);
                        return Err(error);
                    }
                };
                outcome.report.lt_eboot_patch = Some(patch_report);
                staged_eboot = Some(stage_eboot);
            } else if options.eboot_in.is_some()
                || options.force
                || options.charset_map.is_some()
                || options.wrap_width_px.is_some()
                || options.wrap_width_table.is_some()
                || options.wrap_rows.is_some()
                || options.wrap_mode.is_some()
            {
                let _ = fs::remove_file(&staged_output);
                return Err(AssetError::InvalidProject(
                    "--eboot-in/--eboot-out/-f/--charset-map/wrap options are valid only for editable sc.cpk or lt.bin projects".to_owned(),
                ));
            }

            if let Some(portable_output) = portable_metadata_output.as_ref() {
                let prepared = (|| -> Result<Option<PathBuf>, AssetError> {
                    let mut document = match outcome.portable_dialogue_metadata.clone() {
                        Some(document) if codec::script::dialogue_metadata_has_entries(&document) => document,
                        _ => return Ok(None),
                    };
                    document.archive_sha256 = Some(sha256_file_hex(&staged_output)?);
                    let stage_metadata = build_staging_file(portable_output);
                    if stage_metadata.exists() {
                        fs::remove_file(&stage_metadata)?;
                    }
                    fs::write(&stage_metadata, serde_json::to_vec_pretty(&document)?)?;
                    Ok(Some(stage_metadata))
                })();
                match prepared {
                    Ok(path) => staged_portable_metadata = path,
                    Err(error) => {
                        let _ = fs::remove_file(&staged_output);
                        if let Some(path) = staged_eboot.as_ref() {
                            let _ = fs::remove_file(path);
                        }
                        return Err(error);
                    }
                }
            }

            if let Err(error) = fs::rename(&staged_output, output) {
                let _ = fs::remove_file(&staged_output);
                if let Some(path) = staged_eboot.as_ref() {
                    let _ = fs::remove_file(path);
                }
                if let Some(path) = staged_portable_metadata.as_ref() {
                    let _ = fs::remove_file(path);
                }
                return Err(AssetError::Io(error));
            }
            if let (Some(stage_eboot), Some(output_eboot)) =
                (staged_eboot.as_ref(), options.eboot_out.as_ref())
            {
                if let Err(error) = fs::rename(stage_eboot, output_eboot) {
                    let _ = fs::remove_file(output);
                    let _ = fs::remove_file(stage_eboot);
                    if let Some(path) = staged_portable_metadata.as_ref() {
                        let _ = fs::remove_file(path);
                    }
                    return Err(AssetError::Io(error));
                }
            }
            if let (Some(stage_metadata), Some(portable_output)) = (
                staged_portable_metadata.as_ref(),
                portable_metadata_output.as_ref(),
            ) {
                if let Err(error) = fs::rename(stage_metadata, portable_output) {
                    let _ = fs::remove_file(output);
                    if let Some(output_eboot) = options.eboot_out.as_ref() {
                        let _ = fs::remove_file(output_eboot);
                    }
                    let _ = fs::remove_file(stage_metadata);
                    return Err(AssetError::Io(error));
                }
            }
            Ok(outcome.report)
        }
        Err(error) => {
            let _ = fs::remove_file(&staged_output);
            if let Some(path) = staged_portable_metadata.as_ref() {
                let _ = fs::remove_file(path);
            }
            Err(error)
        }
    };
    final_result
}

fn build_cpk(
    project_directory: &Path,
    output_cpk: &Path,
    stage: &Path,
    manifest: &ProjectManifest,
    cpk: &CpkProject,
    assets: &[AssetEntry],
    options: &BuildOptions,
) -> Result<BuildOutcome, AssetError> {
    let mut ordered = assets.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|entry| entry.order);
    let archive = manifest.source_name.to_ascii_lowercase();
    let is_sc = archive == "sc.cpk";
    if is_sc && ordered.len() != SC_ENTRY_COUNT {
        return Err(AssetError::InvalidProject(format!(
            "sc.cpk project contains {} entries, expected {SC_ENTRY_COUNT}",
            ordered.len()
        )));
    }
    let allow_eboot_patch = options.eboot_in.is_some() && options.eboot_out.is_some();
    let charset_map = if is_sc {
        let configured = options
            .charset_map
            .clone()
            .or_else(|| {
                let project_charset = project_directory.join("charset.json");
                project_charset.exists().then_some(project_charset)
            });
        Some(match configured {
            Some(path) => codec::charset::load_map(&path)?,
            None => codec::charset::default_map().clone(),
        })
    } else {
        None
    };
    let scenario_dialogues = if is_sc {
        let mut entries = codec::script::load_dialogue_document(project_directory)?;
        if let Some(width_table) = options.wrap_width_table.as_ref() {
            let limit = options.wrap_width_px.unwrap_or(DEFAULT_WRAP_WIDTH_PX);
            let widths = load_fixed_width_table(width_table)?;
            let mode = options.wrap_mode.unwrap_or_default();
            let rows = options.wrap_rows.unwrap_or(DEFAULT_WRAP_ROWS);
            apply_fixed_width_wrap(
                &mut entries,
                &widths,
                limit,
                mode,
                rows,
            )?;
        }
        if entries.len() != SC_ENTRY_COUNT {
            return Err(AssetError::InvalidProject(format!(
                "scenario-dialogue.json contains {} entries, expected {SC_ENTRY_COUNT}",
                entries.len()
            )));
        }
        let indexed = index_dialogue_entries_by_entry_id(entries)?;
        Some(indexed)
    } else {
        None
    };
    let portable_dialogue_metadata = scenario_dialogues
        .as_ref()
        .map(|entries| codec::script::build_dialogue_metadata_document(entries, None))
        .filter(codec::script::dialogue_metadata_has_entries);
    let scenario_states = if is_sc {
        let entries = codec::script::load_state_bundle(project_directory)?;
        if entries.len() != SC_ENTRY_COUNT {
            return Err(AssetError::InvalidProject(format!(
                ".rz-internal/sc-build-state.json.gz contains {} entries, expected {SC_ENTRY_COUNT}",
                entries.len()
            )));
        }
        Some(index_state_entries_by_entry_id(entries)?)
    } else {
        None
    };
    let mut sc_entries = [ScEntryPatch::default(); SC_ENTRY_COUNT];
    let mut sc_seen = [false; SC_ENTRY_COUNT];
    let mut inputs = Vec::with_capacity(ordered.len());

    for entry in ordered {
        validate_entry_kind(&manifest.mode, &manifest.source_name, entry)?;
        let entry_id = entry.id.unwrap_or(entry.order);
        let mut bytes = if is_sc {
            let AssetKind::Script { document } = &entry.kind else {
                return Err(AssetError::InvalidProject(format!(
                    "sc.cpk entry {} is not a script document",
                    entry.file_name
                )));
            };
            if document != ".rz-internal/sc-build-state.json.gz"
                && document != ".rz-internal/sc-state.json.gz"
            {
                return Err(AssetError::InvalidProject(format!(
                    "sc.cpk entry {} does not reference the compact SC build-state bundle",
                    entry.file_name
                )));
            }
            let index = usize::try_from(entry_id).map_err(|_| {
                AssetError::InvalidProject("SC entry ID exceeds usize".to_owned())
            })?;
            if index >= SC_ENTRY_COUNT || sc_seen[index] {
                return Err(AssetError::InvalidProject(format!(
                    "sc.cpk has an invalid or duplicate entry ID {entry_id}"
                )));
            }
            let dialogue = scenario_dialogues
                .as_ref()
                .and_then(|entries| entries.get(&entry_id))
                .ok_or_else(|| {
                    AssetError::InvalidProject(format!(
                        "scenario-dialogue.json omits engine entry {entry_id}"
                    ))
                })?;
            let state = scenario_states
                .as_ref()
                .and_then(|entries| entries.get(&entry_id))
                .ok_or_else(|| {
                    AssetError::InvalidProject(format!(
                        ".rz-internal/sc-build-state.json.gz omits engine entry {entry_id}"
                    ))
                })?;
            let info = codec::script::inspect_state_build_with_charset_and_dialogue(
                state,
                charset_map.as_ref().expect("SC charset was initialized"),
                Some(dialogue),
            )?;
            let stock_allocation = engine_allocations::allocation_size(&archive, entry_id)
                .ok_or_else(|| {
                    AssetError::InvalidProject(format!(
                        "SC entry ID {entry_id} is absent from the stock sector table"
                    ))
                })?;
            let extracted_allocation = usize::try_from(state.state.allocation_size).map_err(|_| {
                AssetError::InvalidProject(format!(
                    "SC entry {entry_id} extracted allocation size exceeds usize"
                ))
            })?;
            // Modified SC projects may already have larger per-entry allocations
            // recorded in .rz-internal/sc-build-state.json.gz.  Those allocations are
            // part of the extracted executable/runtime contract and must be
            // preserved during rebuild.  Selecting max(stock, required) silently
            // shrinks entries whose rebuilt text currently fits inside stock but
            // whose extracted payload/allocation is larger, which causes
            // encode_document_source() to reject the project with
            // "script source_size exceeds allocation_size" before it can reach
            // the clearer no-shrink invariant.
            let allocation = stock_allocation
                .max(extracted_allocation)
                .max(info.required_allocation);
            if allocation > stock_allocation && !allow_eboot_patch {
                return Err(AssetError::InvalidProject(format!(
                    "SC entry {entry_id} requires {allocation:#x} bytes but stock eboot allocates {stock_allocation:#x}; provide --eboot-in and --eboot-out"
                )));
            }
            let sectors = allocation / engine_allocations::SECTOR_SIZE;
            let sectors = u16::try_from(sectors).map_err(|_| {
                AssetError::InvalidProject(format!(
                    "SC entry {entry_id} requires more than u16 sectors"
                ))
            })?;
            sc_entries[index] = ScEntryPatch {
                sectors,
                stream_count: info.stream_count,
                primary_count: info.primary_count,
                secondary_count: info.secondary_count,
            };
            sc_seen[index] = true;
            codec::script::encode_state_with_allocation_and_charset_and_dialogue(
                state,
                allocation,
                allow_eboot_patch,
                charset_map.as_ref().expect("SC charset was initialized"),
                Some(dialogue),
            )?
        } else {
            codec::encode(&entry.kind, project_directory)?
        };

        if !is_sc {
            if let Some(allocation_size) =
                engine_allocations::allocation_size(&archive, entry_id)
            {
                if bytes.len() > allocation_size {
                    return Err(AssetError::InvalidProject(format!(
                        "{} entry ID {} rebuild is {:#x} bytes, exceeding the engine's fixed {allocation_size:#x}-byte sector allocation",
                        manifest.source_name,
                        entry_id,
                        bytes.len()
                    )));
                }
                bytes.resize(allocation_size, 0);
            }
        }

        let context = DecodeContext {
            archive_name: &manifest.source_name,
            directory: &entry.directory,
            file_name: &entry.file_name,
            order: entry.order,
            id: entry.id,
        };
        codec::ensure_in_scope(&context, &bytes)?;
        let source_path = stage.join(format!("{:05}.entry", entry.order));
        fs::write(&source_path, &bytes)?;
        let size = u32::try_from(bytes.len()).map_err(|_| {
            AssetError::InvalidProject(format!("{} exceeds the CPK u32 size", entry.file_name))
        })?;
        inputs.push(CpkInputFile {
            directory: entry.directory.clone(),
            file_name: entry.file_name.clone(),
            source_path,
            size,
            extract_size: size,
            id: entry.id.unwrap_or(entry.order),
            user_string: entry.user_string.clone(),
            raw_payload: None,
        });
    }

    if is_sc && sc_seen.iter().any(|seen| !seen) {
        return Err(AssetError::InvalidProject(
            "sc.cpk project does not contain every entry ID 0..88".to_owned(),
        ));
    }

    let profile = CpkWriterProfile {
        index_mode: match cpk.index_mode {
            IndexMode::Toc => CpkIndexMode::Toc,
            IndexMode::Itoc => CpkIndexMode::Itoc,
            IndexMode::TocAndItoc => CpkIndexMode::TocAndItoc,
        },
        direct_itoc: cpk.direct_itoc,
        version: cpk.version,
        revision: cpk.revision,
        update_date_time: cpk.update_date_time,
        tvers: format!("{}.{}.0", cpk.version, cpk.revision),
        comment: "Created by rz-tool".to_owned(),
    };
    let writer_options = CpkWriterOptions {
        alignment: cpk.alignment,
        p5r_encryption: false,
    };
    let report = CpkWriter::pack_files_with_profile(output_cpk, &inputs, writer_options, &profile)
        .map_err(AssetError::Archive)?;
    Ok(BuildOutcome {
        report: BuildReport {
            files: report.files,
            output_size: report.archive_size,
            eboot_patch: None,
            lt_eboot_patch: None,
        },
        sc_patch_plan: is_sc.then_some(ScPatchPlan { entries: sc_entries }),
        lt_patch_plan: None,
        portable_dialogue_metadata,
    })
}


fn validate_entry_kind(
    mode: &ProjectMode,
    source_name: &str,
    entry: &AssetEntry,
) -> Result<(), AssetError> {
    let archive = source_name.to_ascii_lowercase();
    match (mode, archive.as_str(), &entry.kind) {
        (ProjectMode::RawOnly, _, AssetKind::Raw { .. }) => Ok(()),
        (ProjectMode::RawOnly, _, _) => Err(AssetError::InvalidProject(format!(
            "raw-only project contains decoded entry {}",
            entry.file_name
        ))),
        (ProjectMode::Editable, "sc.cpk", AssetKind::Script { .. }) => Ok(()),
        (
            ProjectMode::Editable,
            "addpt.cpk" | "bk.cpk" | "bsf.cpk" | "pt.cpk",
            AssetKind::Opaque { .. },
        ) => Ok(()),
        (
            ProjectMode::Editable,
            "addpt.cpk" | "bk.cpk" | "bsf.cpk" | "pt.cpk",
            AssetKind::EngineImagePackage { .. },
        ) => Ok(()),
        (ProjectMode::Editable, "sc.cpk", _) => Err(AssetError::InvalidProject(format!(
            "sc.cpk entry {} must be structured compiled script payload",
            entry.file_name
        ))),
        (
            ProjectMode::Editable,
            "addpt.cpk" | "bk.cpk" | "bsf.cpk" | "pt.cpk",
            _,
        ) => Err(AssetError::InvalidProject(format!(
            "{} entry {} must be an engine image package or an explicit opaque fallback",
            source_name, entry.file_name
        ))),
        (ProjectMode::Editable, _, _) => Err(AssetError::OutOfScopeResource(
            source_name.to_owned(),
        )),
    }
}

fn validate_manifest(manifest: &ProjectManifest) -> Result<(), AssetError> {
    if manifest.schema_version != PROJECT_SCHEMA_VERSION {
        return Err(AssetError::InvalidProject(format!(
            "schema version {} is unsupported; expected {}",
            manifest.schema_version, PROJECT_SCHEMA_VERSION
        )));
    }
    match &manifest.source {
        ProjectSource::Cpk { assets, .. } => {
            if assets.is_empty() {
                return Err(AssetError::InvalidProject(
                    "CPK project contains no assets".to_owned(),
                ));
            }
            let mut orders = assets.iter().map(|entry| entry.order).collect::<Vec<_>>();
            orders.sort_unstable();
            orders.dedup();
            if orders.len() != assets.len() {
                return Err(AssetError::InvalidProject(
                    "duplicate asset order".to_owned(),
                ));
            }
            for entry in assets {
                validate_asset_kind(&entry.kind)?;
            }
        }
        ProjectSource::LtFont { document } => {
            if manifest.mode != ProjectMode::Editable {
                return Err(AssetError::InvalidProject(
                    "lt.bin documents require editable project mode".to_owned(),
                ));
            }
            validate_relative_project_path(document)?;
        }
        ProjectSource::RawFile { path } => {
            if manifest.mode != ProjectMode::RawOnly {
                return Err(AssetError::InvalidProject(
                    "raw-file source requires raw-only project mode".to_owned(),
                ));
            }
            validate_relative_project_path(path)?;
        }
    }
    Ok(())
}

fn validate_asset_kind(kind: &AssetKind) -> Result<(), AssetError> {
    match kind {
        AssetKind::Raw { path }
        | AssetKind::Opaque { path, .. }
        | AssetKind::EngineImagePackage { document: path }
        | AssetKind::Script { document: path } => validate_relative_project_path(path),
        AssetKind::Gxt { images, .. } => {
            for path in images {
                validate_relative_project_path(path)?;
            }
            Ok(())
        }
    }
}

fn validate_relative_project_path(value: &str) -> Result<(), AssetError> {
    let path = Path::new(value);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(AssetError::InvalidProject(format!(
            "asset path must stay inside the project: {value:?}"
        )));
    }
    Ok(())
}

fn write_manifest(stage: &Path, manifest: &ProjectManifest) -> Result<(), AssetError> {
    fs::write(
        stage.join(PROJECT_FILE_NAME),
        serde_json::to_vec_pretty(manifest)?,
    )?;
    Ok(())
}

fn cpk_project(metadata: CpkMetadata) -> CpkProject {
    let index_mode = match (metadata.toc_offset != 0, metadata.itoc_offset != 0) {
        (true, true) => IndexMode::TocAndItoc,
        (false, true) => IndexMode::Itoc,
        _ => IndexMode::Toc,
    };
    CpkProject {
        alignment: metadata.align.max(1),
        index_mode,
        direct_itoc: metadata.eid != 0,
        version: metadata.version,
        revision: metadata.revision,
        update_date_time: metadata.update_date_time,
    }
}

fn safe_component(value: &str) -> String {
    let mut result = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if result.is_empty() || result == "." || result == ".." {
        result = "entry.bin".to_owned();
    }
    result
}

fn staging_directory(output: &Path) -> PathBuf {
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    let name = output
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("project");
    parent.join(format!(".{name}.rz-stage-{}", std::process::id()))
}

fn build_staging_directory(output: &Path) -> PathBuf {
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    let name = output
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("output");
    parent.join(format!(".{name}.rz-build-{}", std::process::id()))
}

fn build_staging_file(output: &Path) -> PathBuf {
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    let name = output
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("output.bin");
    parent.join(format!(".{name}.rz-output-{}", std::process::id()))
}

pub fn describe_error_chain(error: &dyn Error) -> String {
    let mut output = error.to_string();
    let mut source = error.source();
    while let Some(next) = source {
        output.push_str("\n  caused by: ");
        output.push_str(&next.to_string());
        source = next.source();
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit_widths() -> FixedWidthPolicy {
        FixedWidthPolicy {
            explicit: HashMap::new(),
            default_advance: 1,
        }
    }

    #[test]
    fn word_wrap_records_space_only_at_word_boundaries() {
        let wrapped = wrap_text_to_engine_rows_word("aa bb", &unit_widths(), 3).unwrap();
        assert_eq!(wrapped.rows, vec!["aa", "bb"]);
        assert_eq!(wrapped.row_joiners, vec![" "]);
    }

    #[test]
    fn word_wrap_records_empty_joiner_inside_overwide_word() {
        let wrapped = wrap_text_to_engine_rows_word("abcdef", &unit_widths(), 3).unwrap();
        assert_eq!(wrapped.rows, vec!["abc", "def"]);
        assert_eq!(wrapped.row_joiners, vec![""]);
    }
}
