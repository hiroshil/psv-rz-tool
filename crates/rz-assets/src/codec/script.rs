use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};

use crate::codec::charset;
use crate::engine_allocations::{self, ScMetadata};
use crate::error::AssetError;
use crate::manifest::{AssetEntry, AssetKind};

const DOCUMENT_VERSION: u32 = 1;
const SOURCE_DOCUMENT_VERSION: u32 = 1;
const ROUTING_DOCUMENT_VERSION: u32 = 1;
const DIALOGUE_DOCUMENT_VERSION: u32 = 1;
const STATE_BUNDLE_VERSION: u32 = 1;
const STATE_BUNDLE_PATH: &str = ".rz-internal/sc-build-state.json.gz";
const LEGACY_STATE_BUNDLE_PATH: &str = ".rz-internal/sc-state.json.gz";
const LEGACY_DIALOGUE_METADATA_PATH: &str = "scenario-dialogue.meta.json";
const PORTABLE_DIALOGUE_METADATA_SUFFIX: &str = ".rz-dialogue-meta.json";
const VOICE_HEADER_SIZE: usize = 0x80;
const RUNTIME_BASE: usize = 0x80;
const SCRIPT_BASE: usize = 0x2000;
const RUNTIME_HEADER_SIZE: usize = 0x10;
const SECONDARY_RECORD_SIZE: usize = 0x1c;
const SC_INTEGRITY_FOOTER_SIZE: usize = 0x10;
const SC_INTEGRITY_SEED: u64 = 0x1111_1111_1111_1111;
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
    /// integrity footer. Zero fill between logical payload and footer is capacity,
    /// not source IR.
    pub payload_capacity_bytes: u32,
    /// Last 16 bytes of the allocated SC payload. `FUN_8102B4AC` verifies this
    /// as two seeded 64-bit additive checksums over the complete allocation
    /// excluding the footer. The extracted value is retained for diagnostics;
    /// build always regenerates it from rebuilt bytes.
    pub opaque_footer_hex: String,
    /// Rebuild-authoritative representation of entry+0x0000..0x007f.
    pub voice_header: ScriptVoiceHeader,
    /// Machine-managed rebuild IR path. User text edits are applied from
    /// scenario-dialogue.json; this detailed representation remains internal.
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
    pub navigation_model: String,
    pub transition_model: String,
    pub dialogue_document: String,
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
    pub navigation_order: u32,
    pub component_root_entry_id: u32,
    pub route_depth: u32,
    pub reachable_from_startup: bool,
    pub entry_id: u32,
    pub archive_order: u32,
    pub stream_count: u32,
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
pub struct ScenarioDialogueDocument {
    pub document_version: u32,
    pub archive: String,
    pub entries: Vec<ScenarioDialogueEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioDialogueMetadataDocument {
    pub document_version: u32,
    pub archive: String,
    /// Optional archive identity used only by the portable companion sidecar.
    /// Internal build-state dialogue metadata omits it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<ScenarioDialogueMetadataEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioDialogueMetadataEntry {
    pub entry_id: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dialogues: Vec<ScenarioDialogueMetadataItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioDialogueMetadataItem {
    pub marker_index: u32,
    /// Separator text removed from engine row boundaries for cleaner rendering.
    /// This is metadata only: it does not duplicate full dialogue text, row text,
    /// speaker text, wrap width, pixel metrics, or debug lines.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub row_joiners: Vec<String>,
    /// Number of additional generated message markers produced by the build for
    /// this root marker. This records structure, not text or build profile.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub generated_marker_count: u32,
}

fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioDialogueEntry {
    pub entry_id: u32,
    pub dialogues: Vec<ScenarioDialogueItem>,
    #[serde(default)]
    pub texts: Vec<ScenarioTextItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioDialogueItem {
    pub marker_index: u32,
    pub speaker: String,
    /// User-editable dialogue text. Engine line/page splitting is an internal
    /// build detail and is never serialized in this document.
    pub text: String,
    #[serde(skip)]
    engine_lines_override: Option<Vec<String>>,
    #[serde(skip)]
    engine_screens_override: Option<Vec<Vec<String>>>,
    #[serde(skip)]
    row_joiners_override: Option<Vec<String>>,
    #[serde(skip)]
    generated_marker_count_override: Option<u32>,
}

impl ScenarioDialogueItem {
    pub fn engine_lines(&self) -> Vec<String> {
        if let Some(lines) = self.engine_lines_override.as_ref() {
            return lines.clone();
        }
        vec![normalize_user_text_for_build(&self.text)]
    }

    pub fn set_engine_lines_for_build(&mut self, lines: Vec<String>) {
        self.engine_lines_override = Some(lines);
        self.engine_screens_override = None;
        self.row_joiners_override = None;
        self.generated_marker_count_override = None;
    }

    pub fn engine_screens_for_build(&self) -> Vec<Vec<String>> {
        if let Some(screens) = self.engine_screens_override.as_ref() {
            return screens.clone();
        }
        vec![self.engine_lines()]
    }

    pub fn row_joiners_for_build(&self) -> Vec<String> {
        self.row_joiners_override.clone().unwrap_or_default()
    }

    pub fn set_row_joiners_for_build(&mut self, row_joiners: Vec<String>) {
        self.row_joiners_override = Some(row_joiners);
    }

    pub fn generated_marker_count_for_build(&self) -> u32 {
        if let Some(screens) = self.engine_screens_override.as_ref() {
            return screens.len().saturating_sub(1) as u32;
        }
        self.generated_marker_count_override.unwrap_or(0)
    }

    pub fn source_generated_marker_count(&self) -> u32 {
        self.generated_marker_count_override.unwrap_or(0)
    }

    pub fn set_roundtrip_metadata_for_build(
        &mut self,
        row_joiners: Vec<String>,
        generated_marker_count: u32,
    ) {
        self.row_joiners_override = Some(row_joiners);
        self.generated_marker_count_override = Some(generated_marker_count);
    }

    pub fn set_engine_screens_for_build(&mut self, screens: Vec<Vec<String>>) {
        self.engine_screens_override = Some(screens);
        self.engine_lines_override = None;
    }

    pub fn set_engine_screens_for_build_with_joiners(
        &mut self,
        screens: Vec<Vec<String>>,
        row_joiners: Vec<String>,
    ) {
        self.engine_screens_override = Some(screens);
        self.engine_lines_override = None;
        self.row_joiners_override = Some(row_joiners);
    }
}

fn normalize_user_text_for_build(text: &str) -> String {
    let mut out = String::new();
    let mut pending_space = false;
    for ch in text.chars() {
        if ch == '\r' || ch == '\n' || ch == '\t' {
            pending_space = true;
            continue;
        }
        if pending_space && !out.is_empty() && ch != ' ' {
            out.push(' ');
        }
        pending_space = false;
        out.push(ch);
    }
    out.trim_matches(|ch| ch == ' ' || ch == '\t' || ch == '\r' || ch == '\n').to_owned()
}


fn source_dialogue_matches_edit(
    edit: &ScenarioDialogueItem,
    speaker: &str,
    pages: &[ScriptDialoguePage],
) -> bool {
    if edit.speaker.as_str() != speaker {
        return false;
    }
    let joiners = edit.row_joiners_for_build();
    let source_text = merge_engine_lines_to_user_text_with_joiners(
        pages.iter().map(|page| page.text.as_str()),
        &joiners,
    );
    normalize_user_text_for_build(&edit.text) == source_text
}

fn split_text_by_machine_page_counts(
    text: &str,
    page_char_counts: &[u32],
    row_joiners: &[String],
) -> Option<Vec<String>> {
    if page_char_counts.is_empty() {
        return None;
    }
    let characters = text.chars().collect::<Vec<_>>();
    let mut position = 0usize;
    let mut pages = Vec::with_capacity(page_char_counts.len());
    for (row_index, count) in page_char_counts.iter().copied().enumerate() {
        let count = usize::try_from(count).ok()?;
        let end = position.checked_add(count)?;
        if end > characters.len() {
            return None;
        }
        pages.push(characters[position..end].iter().collect::<String>());
        position = end;
        if row_index + 1 < page_char_counts.len() {
            let joiner = row_joiners.get(row_index).map(String::as_str).unwrap_or("");
            let joiner_len = joiner.chars().count();
            let joiner_end = position.checked_add(joiner_len)?;
            if joiner_end > characters.len() {
                return None;
            }
            let actual = characters[position..joiner_end]
                .iter()
                .collect::<String>();
            if actual != joiner {
                return None;
            }
            position = joiner_end;
        }
    }
    (position == characters.len()).then_some(pages)
}

fn rebuild_machine_pages_from_texts(
    page_texts: &[String],
    page_glyph_aliases: &[Vec<ScriptGlyphAlias>],
    charset_map: &charset::CharsetMap,
) -> Result<Vec<ScriptDialoguePage>, AssetError> {
    page_texts
        .iter()
        .enumerate()
        .map(|(row_index, text)| {
            let aliases = page_glyph_aliases
                .get(row_index)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            Ok(ScriptDialoguePage {
                text: text.clone(),
                source_glyphs: encode_text_with_aliases(text, aliases, charset_map)?,
            })
        })
        .collect()
}

fn remap_dialogue_invocation_words(
    words: &mut [u16],
    marker_remap: &BTreeMap<u32, u32>,
) -> Result<(), AssetError> {
    let mut index = 0usize;
    while index + 1 < words.len() {
        if words[index] == 0xff68 {
            let old_marker = u32::from(words[index + 1]);
            if let Some(new_marker) = marker_remap.get(&old_marker) {
                words[index + 1] = u16::try_from(*new_marker).map_err(|_| {
                    AssetError::InvalidProject("dialogue marker reference exceeds u16".to_owned())
                })?;
            }
            index += 2;
        } else {
            index += 1;
        }
    }
    Ok(())
}

fn clone_simple_dialogue_invocation(
    words: &[u16],
    original_marker: u32,
    new_marker: u32,
) -> Result<Vec<u16>, AssetError> {
    if words.len() != 4 || words[0] != 0xfffb || words[1] != 0xff68 {
        return Err(AssetError::InvalidProject(format!(
            "dialogue marker {original_marker} wraps beyond one message entry, but its VM invocation is not the proven simple `FFFB FF68 marker arg` form"
        )));
    }
    if u32::from(words[2]) != original_marker {
        return Err(AssetError::InvalidProject(format!(
            "dialogue marker {original_marker} wraps beyond one message entry, but the following simple VM invocation targets marker {}",
            words[2]
        )));
    }
    let mut cloned = words.to_vec();
    cloned[2] = u16::try_from(new_marker).map_err(|_| {
        AssetError::InvalidProject("dialogue continuation marker exceeds u16".to_owned())
    })?;
    Ok(cloned)
}

fn merge_engine_lines_to_user_text_with_joiners<'a, I>(lines: I, row_joiners: &[String]) -> String
where
    I: IntoIterator<Item = &'a str>,
{
    let parts = lines
        .into_iter()
        .map(|raw| raw.trim_matches(|ch| ch == ' ' || ch == '\t' || ch == '\r' || ch == '\n'))
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    let mut out = String::new();
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            if let Some(joiner) = row_joiners.get(index - 1) {
                out.push_str(joiner);
            } else if let (Some(prev), Some(next)) = (out.chars().last(), part.chars().next()) {
                if should_insert_user_text_space(prev, next) {
                    out.push(' ');
                }
            }
        }
        out.push_str(part);
    }
    out
}

fn derive_heuristic_row_joiners<'a, I>(lines: I) -> Vec<String>
where
    I: IntoIterator<Item = &'a str>,
{
    let parts = lines
        .into_iter()
        .map(|raw| raw.trim_matches(|ch| ch == ' ' || ch == '\t' || ch == '\r' || ch == '\n'))
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    parts
        .windows(2)
        .map(|window| {
            let left = window[0].chars().last();
            let right = window[1].chars().next();
            if let (Some(prev), Some(next)) = (left, right) {
                if should_insert_user_text_space(prev, next) {
                    return " ".to_owned();
                }
            }
            String::new()
        })
        .collect()
}

fn should_insert_user_text_space(prev: char, next: char) -> bool {
    if prev.is_whitespace() || next.is_whitespace() {
        return false;
    }
    if matches!(next, ',' | '.' | ':' | ';' | '!' | '?' | '、' | '。' | '，' | '．' | '：' | '；' | '！' | '？' | ')' | ']' | '}' | '」' | '』' | '）' | '】') {
        return false;
    }
    if matches!(prev, '(' | '[' | '{' | '「' | '『' | '（' | '【') {
        return false;
    }
    if matches!(prev, '.' | ':' | ';' | '!' | '?' | '。' | '．' | '：' | '；' | '！' | '？')
        && is_space_separated_script(next)
    {
        return true;
    }
    if (is_space_separated_script(prev) && is_cjk_or_kana_script(next))
        || (is_cjk_or_kana_script(prev) && is_space_separated_script(next))
    {
        return true;
    }
    is_space_separated_script(prev) && is_space_separated_script(next)
}

fn is_space_separated_script(ch: char) -> bool {
    ch.is_ascii_alphanumeric()
        || ('\u{00c0}'..='\u{024f}').contains(&ch)
        || ('\u{1e00}'..='\u{1eff}').contains(&ch)
}

fn is_cjk_or_kana_script(ch: char) -> bool {
    ('\u{3040}'..='\u{30ff}').contains(&ch)
        || ('\u{3400}'..='\u{4dbf}').contains(&ch)
        || ('\u{4e00}'..='\u{9fff}').contains(&ch)
        || ('\u{f900}'..='\u{faff}').contains(&ch)
        || ('\u{ff66}'..='\u{ff9f}').contains(&ch)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioTextItem {
    pub text_index: u32,
    pub grammar: ScriptTextGrammar,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioStateBundle {
    pub document_version: u32,
    pub archive: String,
    /// Internal route/navigation report formerly emitted as scenario-routing.json.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing: Option<ScenarioRoutingDocument>,
    /// Internal row-joiner/continuation metadata formerly emitted as scenario-dialogue.meta.json.
    /// This contains marker structure only; user-visible dialogue text and speakers remain only in scenario-dialogue.json.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dialogue_metadata: Option<ScenarioDialogueMetadataDocument>,
    pub entries: Vec<ScenarioStateEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioStateEntry {
    pub entry_id: u32,
    pub state: ScriptMachineState,
    /// Structural and relocation state only. All user-visible text lives once,
    /// in scenario-dialogue.json; this document contains no Unicode strings or
    /// complete source-glyph sequences for dialogue text.
    pub source: ScriptMachineDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptMachineState {
    pub document_version: u32,
    pub entry_id: u32,
    pub source_size: u32,
    pub allocation_size: u32,
    pub payload_capacity_bytes: u32,
    pub opaque_footer_hex: String,
    pub voice_header: ScriptVoiceHeader,
    pub trailing_byte: Option<u8>,
    pub secondary_records: Vec<ScriptSourceSecondaryRecord>,
    pub engine_metadata: Option<ScriptEngineMetadataAnnotation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptMachineDocument {
    pub document_version: u32,
    pub entry_id: u32,
    pub stream_count: u32,
    pub nodes: Vec<ScriptMachineNode>,
    pub relocation_model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ScriptMachineNode {
    Raw {
        #[serde(default)]
        labels: Vec<String>,
        words: Vec<u16>,
    },
    Text {
        #[serde(default)]
        labels: Vec<String>,
        text_index: u32,
        grammar: ScriptTextGrammar,
        prefix_words: Vec<u16>,
        suffix_words: Vec<u16>,
        /// Sparse differences from charset.json canonical encoding. This is
        /// required only for genuine glyph aliases and does not duplicate text.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        glyph_aliases: Vec<ScriptGlyphAlias>,
    },
    Dialogue {
        #[serde(default)]
        labels: Vec<String>,
        marker_index: u32,
        /// Sparse glyph alias data for the speaker. The actual speaker string lives only in scenario-dialogue.json.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        speaker_glyph_aliases: Vec<ScriptGlyphAlias>,
        /// Original physical row character counts. This is structural rebuild
        /// state only; the row strings themselves live once in
        /// scenario-dialogue.json and are reconstructed from these counts plus
        /// row-joiner metadata when the stock row layout can be reused.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        page_char_counts: Vec<u32>,
        /// One sparse alias list per extracted page. Added/removed pages simply
        /// have no aliases and are encoded canonically.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        page_glyph_aliases: Vec<Vec<ScriptGlyphAlias>>,
        /// 1-based page counts after which the engine script contains FFFB before
        /// continuing the same dialogue marker.  Generated continuations repeat
        /// non-empty speaker metadata after FFFB so later screens keep the nameplate.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        screen_break_after_pages: Vec<u32>,
        /// 1-based page counts after which FFFB is followed by a repeated
        /// speaker + FFFF refresh.  This is separate from screen breaks so
        /// stock/no-refresh scripts round-trip without synthetic metadata.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        screen_speaker_refresh_after_pages: Vec<u32>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptGlyphAlias {
    pub character_index: u32,
    pub glyph_id: u16,
}

#[derive(Debug, Clone, Copy)]
struct ScenarioNavigationPlacement {
    navigation_order: u32,
    component_root_entry_id: u32,
    route_depth: u32,
    reachable_from_startup: bool,
    entry_id: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ScriptSourceNode {
    Raw {
        #[serde(default)]
        labels: Vec<String>,
        words: Vec<u16>,
    },
    Text {
        #[serde(default)]
        labels: Vec<String>,
        text_index: u32,
        grammar: ScriptTextGrammar,
        prefix_words: Vec<u16>,
        text: String,
        source_glyphs: Vec<u16>,
        suffix_words: Vec<u16>,
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
        /// Ordered physical row bodies. Each row is terminated by FFFE in the
        /// compiled stream.
        pages: Vec<ScriptDialoguePage>,
        /// 1-based row/page counts after which the compiled stream contains FFFB.
        /// Runtime testing showed FFFB is the screen-advance boundary; a fourth
        /// FFFE without this command reuses and overwrites the three row slots.
        /// When `speaker` is non-empty, build repeats `speaker` + FFFF after each
        /// generated FFFB so continuation screens redraw the nameplate.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        screen_break_after_pages: Vec<u32>,
        /// 1-based page counts after which a generated FFFB continuation repeats
        /// the speaker/nameplate metadata before more dialogue rows.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        screen_speaker_refresh_after_pages: Vec<u32>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptDialoguePage {
    pub text: String,
    /// Original glyph IDs for byte-exact no-edit rebuild. If `text` changes,
    /// the assembler ignores this field and encodes the edited Unicode string.
    pub source_glyphs: Vec<u16>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ScriptTextGrammar {
    SecondaryString,
    InlineFf42,
    InlineFf8c,
}

impl ScriptSourceNode {
    fn labels(&self) -> &[String] {
        match self {
            Self::Raw { labels, .. }
            | Self::Text { labels, .. }
            | Self::Dialogue { labels, .. } => labels,
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


fn collect_glyph_aliases(
    text: &str,
    source_glyphs: &[u16],
    charset_map: &charset::CharsetMap,
) -> Result<Vec<ScriptGlyphAlias>, AssetError> {
    if charset_map.decode_slice(source_glyphs)? != text {
        return Err(AssetError::InvalidProject(
            "internal script text does not match its source glyph IDs".to_owned(),
        ));
    }
    let canonical = charset_map.encode_string(text)?;
    if canonical.len() != source_glyphs.len() {
        return Err(AssetError::InvalidProject(
            "internal script text/glyph length mismatch".to_owned(),
        ));
    }
    canonical
        .into_iter()
        .zip(source_glyphs.iter().copied())
        .enumerate()
        .filter_map(|(index, (canonical_id, source_id))| {
            (canonical_id != source_id).then_some((index, source_id))
        })
        .map(|(index, glyph_id)| {
            Ok(ScriptGlyphAlias {
                character_index: u32::try_from(index).map_err(|_| {
                    AssetError::InvalidProject(
                        "script text character index exceeds u32".to_owned(),
                    )
                })?,
                glyph_id,
            })
        })
        .collect()
}

fn encode_text_with_aliases(
    text: &str,
    aliases: &[ScriptGlyphAlias],
    charset_map: &charset::CharsetMap,
) -> Result<Vec<u16>, AssetError> {
    let characters = text.chars().collect::<Vec<_>>();
    let mut glyphs = charset_map.encode_string(text)?;
    let mut seen = BTreeSet::new();
    for alias in aliases {
        let index = usize::try_from(alias.character_index).map_err(|_| {
            AssetError::InvalidProject("script glyph alias index exceeds usize".to_owned())
        })?;
        if !seen.insert(index) {
            return Err(AssetError::InvalidProject(format!(
                "script text contains duplicate glyph alias index {index}"
            )));
        }
        let Some(&character) = characters.get(index) else {
            // An edit shortened or otherwise changed the string. The stale
            // alias is not authoritative text and must not block the edit.
            continue;
        };
        if charset_map.decode_glyph(alias.glyph_id)? == character {
            glyphs[index] = alias.glyph_id;
        }
    }
    Ok(glyphs)
}

fn compact_script_state(document: &ScriptDocument) -> ScriptMachineState {
    ScriptMachineState {
        document_version: document.document_version,
        entry_id: document.entry_id,
        source_size: document.source_size,
        allocation_size: document.allocation_size,
        payload_capacity_bytes: document.payload_capacity_bytes,
        opaque_footer_hex: document.opaque_footer_hex.clone(),
        voice_header: document.voice_header.clone(),
        trailing_byte: document.trailing_byte,
        secondary_records: document.secondary_records.clone(),
        engine_metadata: document.engine_metadata.clone(),
    }
}

fn hydrate_script_state(state: &ScriptMachineState) -> ScriptDocument {
    ScriptDocument {
        document_version: state.document_version,
        entry_id: state.entry_id,
        source_size: state.source_size,
        allocation_size: state.allocation_size,
        payload_capacity_bytes: state.payload_capacity_bytes,
        opaque_footer_hex: state.opaque_footer_hex.clone(),
        voice_header: state.voice_header.clone(),
        editable: "scenario-dialogue.json".to_owned(),
        trailing_byte: state.trailing_byte,
        secondary_records: state.secondary_records.clone(),
        engine_metadata: state.engine_metadata.clone(),
        stream_table: ScriptOffsetTableAnnotation {
            table_bytes: 0,
            entries: Vec::new(),
        },
        primary_markers: ScriptPrimaryMarkerAnnotation {
            marker_word: "FFF0".to_owned(),
            derived_count: 0,
            offsets: Vec::new(),
            generation: "derived during build from compact SC state".to_owned(),
        },
        resource_routing: resource_routing(),
        limitations: Vec::new(),
    }
}

fn validate_machine_state(state: &ScriptMachineState) -> Result<(), AssetError> {
    if state.document_version != DOCUMENT_VERSION {
        return Err(AssetError::InvalidProject(format!(
            "SC machine-state metadata version {} is unsupported",
            state.document_version
        )));
    }
    Ok(())
}

fn compact_source_document(
    source: &ScriptSourceDocument,
    charset_map: &charset::CharsetMap,
) -> Result<ScriptMachineDocument, AssetError> {
    if source.document_version != SOURCE_DOCUMENT_VERSION
        || source.charset != charset_map.id()
    {
        return Err(AssetError::InvalidProject(
            "internal script IR version or charset is invalid while compacting SC state"
                .to_owned(),
        ));
    }
    let nodes = source
        .nodes
        .iter()
        .map(|node| match node {
            ScriptSourceNode::Raw { labels, words } => Ok(ScriptMachineNode::Raw {
                labels: labels.clone(),
                words: words.clone(),
            }),
            ScriptSourceNode::Text {
                labels,
                text_index,
                grammar,
                prefix_words,
                text,
                source_glyphs,
                suffix_words,
            } => Ok(ScriptMachineNode::Text {
                labels: labels.clone(),
                text_index: *text_index,
                grammar: *grammar,
                prefix_words: prefix_words.clone(),
                suffix_words: suffix_words.clone(),
                glyph_aliases: collect_glyph_aliases(text, source_glyphs, charset_map)?,
            }),
            ScriptSourceNode::Dialogue {
                labels,
                marker_index,
                speaker,
                speaker_source_glyphs,
                pages,
                screen_break_after_pages,
                screen_speaker_refresh_after_pages,
            } => Ok(ScriptMachineNode::Dialogue {
                labels: labels.clone(),
                marker_index: *marker_index,
                speaker_glyph_aliases: collect_glyph_aliases(
                    speaker,
                    speaker_source_glyphs,
                    charset_map,
                )?,
                page_char_counts: pages
                    .iter()
                    .map(|page| {
                        u32::try_from(page.text.chars().count()).map_err(|_| {
                            AssetError::InvalidProject(
                                "dialogue page text length exceeds u32".to_owned(),
                            )
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                page_glyph_aliases: pages
                    .iter()
                    .map(|page| {
                        collect_glyph_aliases(&page.text, &page.source_glyphs, charset_map)
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                screen_break_after_pages: screen_break_after_pages.clone(),
                screen_speaker_refresh_after_pages: screen_speaker_refresh_after_pages.clone(),
            }),
        })
        .collect::<Result<Vec<_>, AssetError>>()?;
    Ok(ScriptMachineDocument {
        document_version: SOURCE_DOCUMENT_VERSION,
        entry_id: source.entry_id,
        stream_count: source.stream_count,
        nodes,
        relocation_model: source.relocation_model.clone(),
    })
}

fn validate_machine_document(source: &ScriptMachineDocument) -> Result<(), AssetError> {
    if source.document_version != SOURCE_DOCUMENT_VERSION {
        return Err(AssetError::InvalidProject(format!(
            "SC machine-state source version {} is unsupported",
            source.document_version
        )));
    }
    if source.stream_count == 0 || source.stream_count > u32::from(u16::MAX) {
        return Err(AssetError::InvalidProject(
            "SC machine-state stream count is invalid".to_owned(),
        ));
    }
    let mut markers = BTreeSet::new();
    let mut texts = BTreeSet::new();
    for node in &source.nodes {
        match node {
            ScriptMachineNode::Dialogue { marker_index, .. } => {
                if !markers.insert(*marker_index) {
                    return Err(AssetError::InvalidProject(format!(
                        "SC machine state entry {} contains duplicate marker {}",
                        source.entry_id, marker_index
                    )));
                }
            }
            ScriptMachineNode::Text { text_index, .. } => {
                if !texts.insert(*text_index) {
                    return Err(AssetError::InvalidProject(format!(
                        "SC machine state entry {} contains duplicate text index {}",
                        source.entry_id, text_index
                    )));
                }
            }
            ScriptMachineNode::Raw { .. } => {}
        }
    }
    Ok(())
}

fn machine_simple_dialogue_invocation_target(node: &ScriptMachineNode) -> Option<u32> {
    let ScriptMachineNode::Raw { words, .. } = node else {
        return None;
    };
    (words.len() == 4 && words[0] == 0xfffb && words[1] == 0xff68)
        .then_some(u32::from(words[2]))
}

fn hydrate_machine_source(
    source: &ScriptMachineDocument,
    edited: &ScenarioDialogueEntry,
    secondary_records: &[ScriptSourceSecondaryRecord],
    charset_map: &charset::CharsetMap,
) -> Result<ScriptSourceDocument, AssetError> {
    validate_machine_document(source)?;
    if source.entry_id != edited.entry_id {
        return Err(AssetError::InvalidProject(format!(
            "scenario dialogue entry {} does not match SC machine-state entry {}",
            edited.entry_id, source.entry_id
        )));
    }
    let mut by_marker = edited
        .dialogues
        .iter()
        .map(|dialogue| (dialogue.marker_index, dialogue))
        .collect::<BTreeMap<_, _>>();
    let mut by_text = edited
        .texts
        .iter()
        .map(|item| (item.text_index, item))
        .collect::<BTreeMap<_, _>>();

    let physical_dialogue_count = source
        .nodes
        .iter()
        .filter(|node| matches!(node, ScriptMachineNode::Dialogue { .. }))
        .count();
    let source_generated_total = edited.dialogues.iter().try_fold(0usize, |total, dialogue| {
        total
            .checked_add(usize::try_from(dialogue.source_generated_marker_count()).map_err(|_| {
                AssetError::InvalidProject(
                    "dialogue continuation count exceeds usize".to_owned(),
                )
            })?)
            .ok_or_else(|| {
                AssetError::InvalidProject("dialogue continuation count overflows".to_owned())
            })
    })?;
    let expected_logical_count = physical_dialogue_count
        .checked_sub(source_generated_total)
        .ok_or_else(|| {
            AssetError::InvalidProject(format!(
                "internal SC dialogue metadata for entry {} describes more generated markers than machine state contains",
                source.entry_id
            ))
        })?;
    if by_marker.len() != expected_logical_count {
        return Err(AssetError::InvalidProject(format!(
            "scenario-dialogue.json entry {} contains {} logical dialogue markers, expected {} after folding portable continuation metadata",
            source.entry_id,
            by_marker.len(),
            expected_logical_count
        )));
    }

    let mut nodes = Vec::with_capacity(source.nodes.len());
    let mut next_marker_index = 0u32;
    let mut logical_marker_index = 0u32;
    let mut marker_remap = BTreeMap::<u32, u32>::new();
    let mut node_index = 0usize;
    while node_index < source.nodes.len() {
        match &source.nodes[node_index] {
            ScriptMachineNode::Raw { labels, words } => {
                let mut words = words.clone();
                remap_dialogue_invocation_words(&mut words, &marker_remap)?;
                nodes.push(ScriptSourceNode::Raw {
                    labels: labels.clone(),
                    words,
                });
                node_index += 1;
            }
            ScriptMachineNode::Text {
                labels,
                text_index,
                grammar,
                prefix_words,
                suffix_words,
                glyph_aliases,
            } => {
                let edit = by_text.remove(text_index).ok_or_else(|| {
                    AssetError::InvalidProject(format!(
                        "scenario-dialogue.json entry {} omits text record {}",
                        source.entry_id, text_index
                    ))
                })?;
                if edit.grammar != *grammar {
                    return Err(AssetError::InvalidProject(format!(
                        "scenario-dialogue.json entry {} text record {} changes grammar from {:?} to {:?}",
                        source.entry_id, text_index, grammar, edit.grammar
                    )));
                }
                nodes.push(ScriptSourceNode::Text {
                    labels: labels.clone(),
                    text_index: *text_index,
                    grammar: *grammar,
                    prefix_words: prefix_words.clone(),
                    text: edit.text.clone(),
                    source_glyphs: encode_text_with_aliases(
                        &edit.text,
                        glyph_aliases,
                        charset_map,
                    )?,
                    suffix_words: suffix_words.clone(),
                });
                node_index += 1;
            }
            ScriptMachineNode::Dialogue {
                marker_index: physical_root_marker,
                ..
            } => {
                let edit = by_marker.remove(&logical_marker_index).ok_or_else(|| {
                    AssetError::InvalidProject(format!(
                        "scenario-dialogue.json entry {} omits logical marker {}",
                        source.entry_id, logical_marker_index
                    ))
                })?;
                let old_generated_count = usize::try_from(edit.source_generated_marker_count())
                    .map_err(|_| {
                        AssetError::InvalidProject(
                            "dialogue continuation count exceeds usize".to_owned(),
                        )
                    })?;
                let old_screen_count = old_generated_count.checked_add(1).ok_or_else(|| {
                    AssetError::InvalidProject("dialogue continuation count overflows".to_owned())
                })?;
                let mut old_screen_indexes = Vec::<usize>::with_capacity(old_screen_count);
                let mut old_screen_counts = Vec::<Vec<u32>>::with_capacity(old_screen_count);
                let mut old_page_char_counts = Vec::<u32>::new();

                for screen_offset in 0..old_screen_count {
                    let dialogue_index = if old_generated_count == 0 {
                        node_index
                    } else {
                        node_index
                            .checked_add(screen_offset.checked_mul(2).ok_or_else(|| {
                                AssetError::InvalidProject(
                                    "dialogue continuation node index overflows".to_owned(),
                                )
                            })?)
                            .ok_or_else(|| {
                                AssetError::InvalidProject(
                                    "dialogue continuation node index overflows".to_owned(),
                                )
                            })?
                    };
                    let Some(ScriptMachineNode::Dialogue {
                        marker_index,
                        page_char_counts,
                        ..
                    }) = source.nodes.get(dialogue_index)
                    else {
                        return Err(AssetError::InvalidProject(format!(
                            "portable dialogue metadata does not match machine state entry {} logical marker {}",
                            source.entry_id, logical_marker_index
                        )));
                    };
                    let expected_marker = physical_root_marker
                        .checked_add(u32::try_from(screen_offset).map_err(|_| {
                            AssetError::InvalidProject(
                                "dialogue continuation count exceeds u32".to_owned(),
                            )
                        })?)
                        .ok_or_else(|| {
                            AssetError::InvalidProject(
                                "dialogue continuation marker overflows".to_owned(),
                            )
                        })?;
                    if *marker_index != expected_marker {
                        return Err(AssetError::InvalidProject(format!(
                            "portable dialogue metadata expects machine marker {}, found {} in entry {}",
                            expected_marker, marker_index, source.entry_id
                        )));
                    }
                    if old_generated_count > 0 {
                        let invocation_index = dialogue_index.checked_add(1).ok_or_else(|| {
                            AssetError::InvalidProject(
                                "dialogue continuation node index overflows".to_owned(),
                            )
                        })?;
                        if source
                            .nodes
                            .get(invocation_index)
                            .and_then(machine_simple_dialogue_invocation_target)
                            != Some(*marker_index)
                        {
                            return Err(AssetError::InvalidProject(format!(
                                "portable dialogue metadata does not match machine state entry {}: marker {} is not followed by `FFFB FF68 marker arg`",
                                source.entry_id, marker_index
                            )));
                        }
                    }
                    old_screen_counts.push(page_char_counts.clone());
                    old_page_char_counts.extend(page_char_counts.iter().copied());
                    old_screen_indexes.push(dialogue_index);
                }

                let row_joiners = edit.row_joiners_for_build();
                let stock_pages = split_text_by_machine_page_counts(
                    &normalize_user_text_for_build(&edit.text),
                    &old_page_char_counts,
                    &row_joiners,
                );
                let stock_screens = stock_pages.as_ref().map(|pages| {
                    let mut offset = 0usize;
                    old_screen_counts
                        .iter()
                        .map(|counts| {
                            let end = offset.saturating_add(counts.len());
                            let screen = pages[offset..end].to_vec();
                            offset = end;
                            screen
                        })
                        .collect::<Vec<_>>()
                });
                let stock_unchanged = stock_screens
                    .as_ref()
                    .map(|screens| {
                        edit.engine_screens_override
                            .as_ref()
                            .map(|overrides| overrides == screens)
                            .unwrap_or(true)
                    })
                    .unwrap_or(false);
                if stock_unchanged {
                    for (screen_index, &dialogue_index) in old_screen_indexes.iter().enumerate() {
                        let ScriptMachineNode::Dialogue {
                            labels,
                            marker_index,
                            speaker_glyph_aliases,
                            page_char_counts: _,
                            page_glyph_aliases,
                            screen_break_after_pages,
                            screen_speaker_refresh_after_pages,
                        } = &source.nodes[dialogue_index]
                        else {
                            unreachable!("validated generated dialogue chain");
                        };
                        let current_marker = next_marker_index;
                        marker_remap.insert(*marker_index, current_marker);
                        nodes.push(ScriptSourceNode::Dialogue {
                            labels: labels.clone(),
                            marker_index: current_marker,
                            speaker: edit.speaker.clone(),
                            speaker_source_glyphs: encode_text_with_aliases(
                                &edit.speaker,
                                speaker_glyph_aliases,
                                charset_map,
                            )?,
                            pages: rebuild_machine_pages_from_texts(
                                &stock_screens
                                    .as_ref()
                                    .expect("stock screens were reconstructed")[screen_index],
                                page_glyph_aliases,
                                charset_map,
                            )?,
                            screen_break_after_pages: screen_break_after_pages.clone(),
                            screen_speaker_refresh_after_pages:
                                screen_speaker_refresh_after_pages.clone(),
                        });
                        if old_generated_count > 0 {
                            let ScriptMachineNode::Raw { labels, words } =
                                &source.nodes[dialogue_index + 1]
                            else {
                                unreachable!("validated generated invocation chain");
                            };
                            let mut words = words.clone();
                            remap_dialogue_invocation_words(&mut words, &marker_remap)?;
                            nodes.push(ScriptSourceNode::Raw {
                                labels: labels.clone(),
                                words,
                            });
                        }
                        next_marker_index = next_marker_index.checked_add(1).ok_or_else(|| {
                            AssetError::InvalidProject(
                                "dialogue marker count overflows".to_owned(),
                            )
                        })?;
                    }
                    node_index += if old_generated_count > 0 {
                        old_screen_count.checked_mul(2).ok_or_else(|| {
                            AssetError::InvalidProject(
                                "dialogue continuation node count overflows".to_owned(),
                            )
                        })?
                    } else {
                        1
                    };
                    logical_marker_index = logical_marker_index.checked_add(1).ok_or_else(|| {
                        AssetError::InvalidProject("logical marker count overflows".to_owned())
                    })?;
                    continue;
                }

                if edit.engine_screens_override.is_none()
                    && (edit.source_generated_marker_count() > 0
                        || !edit.row_joiners_for_build().is_empty())
                {
                    return Err(AssetError::InvalidProject(format!(
                        "scenario-dialogue.json entry {} logical marker {} was edited after fold-back; rebuild with the same --wrap-width-table/profile so continuation and row-joiner metadata can be regenerated",
                        source.entry_id, logical_marker_index
                    )));
                }

                let screens = edit.engine_screens_for_build();
                if screens.is_empty() {
                    return Err(AssetError::InvalidProject(format!(
                        "dialogue marker {} materialized no message screens",
                        logical_marker_index
                    )));
                }
                let invocation_template = if old_generated_count > 0 || screens.len() > 1 {
                    let Some(ScriptMachineNode::Raw { labels, words }) =
                        source.nodes.get(node_index + 1)
                    else {
                        return Err(AssetError::InvalidProject(format!(
                            "dialogue marker {} wraps beyond one message entry, but no following VM invocation node is available",
                            logical_marker_index
                        )));
                    };
                    if machine_simple_dialogue_invocation_target(&source.nodes[node_index + 1])
                        != Some(*physical_root_marker)
                    {
                        return Err(AssetError::InvalidProject(format!(
                            "dialogue marker {} wraps beyond one message entry, but its VM invocation is not the proven simple `FFFB FF68 marker arg` form",
                            logical_marker_index
                        )));
                    }
                    Some((labels.clone(), words.clone()))
                } else {
                    None
                };

                let first_new_marker = next_marker_index;
                let new_screen_count = screens.len();
                let last_new_marker = first_new_marker
                    .checked_add(u32::try_from(new_screen_count.saturating_sub(1)).map_err(|_| {
                        AssetError::InvalidProject(
                            "dialogue screen count exceeds u32".to_owned(),
                        )
                    })?)
                    .ok_or_else(|| {
                        AssetError::InvalidProject("dialogue marker count overflows".to_owned())
                    })?;
                for screen_offset in 0..old_screen_count {
                    let old_marker = physical_root_marker
                        .checked_add(u32::try_from(screen_offset).map_err(|_| {
                            AssetError::InvalidProject(
                                "dialogue continuation count exceeds u32".to_owned(),
                            )
                        })?)
                        .ok_or_else(|| {
                            AssetError::InvalidProject(
                                "dialogue continuation marker overflows".to_owned(),
                            )
                        })?;
                    let mapped = first_new_marker
                        .checked_add(u32::try_from(screen_offset.min(new_screen_count.saturating_sub(1))).map_err(|_| {
                            AssetError::InvalidProject(
                                "dialogue continuation count exceeds u32".to_owned(),
                            )
                        })?)
                        .unwrap_or(last_new_marker)
                        .min(last_new_marker);
                    marker_remap.insert(old_marker, mapped);
                }

                let ScriptMachineNode::Dialogue {
                    labels: root_labels,
                    speaker_glyph_aliases,
                    ..
                } = &source.nodes[node_index]
                else {
                    unreachable!("current node is dialogue");
                };
                for (screen_index, screen_rows) in screens.iter().enumerate() {
                    let current_marker = next_marker_index;
                    let pages = screen_rows
                        .iter()
                        .map(|text| {
                            Ok(ScriptDialoguePage {
                                text: text.clone(),
                                source_glyphs: encode_text_with_aliases(text, &[], charset_map)?,
                            })
                        })
                        .collect::<Result<Vec<_>, AssetError>>()?;
                    nodes.push(ScriptSourceNode::Dialogue {
                        labels: if screen_index == 0 {
                            root_labels.clone()
                        } else {
                            Vec::new()
                        },
                        marker_index: current_marker,
                        speaker: edit.speaker.clone(),
                        speaker_source_glyphs: encode_text_with_aliases(
                            &edit.speaker,
                            speaker_glyph_aliases,
                            charset_map,
                        )?,
                        pages,
                        screen_break_after_pages: Vec::new(),
                        screen_speaker_refresh_after_pages: Vec::new(),
                    });
                    if let Some((raw_labels, words)) = invocation_template.as_ref() {
                        nodes.push(ScriptSourceNode::Raw {
                            labels: if screen_index == 0 {
                                raw_labels.clone()
                            } else {
                                Vec::new()
                            },
                            words: clone_simple_dialogue_invocation(
                                words,
                                *physical_root_marker,
                                current_marker,
                            )?,
                        });
                    }
                    next_marker_index = next_marker_index.checked_add(1).ok_or_else(|| {
                        AssetError::InvalidProject("dialogue marker count overflows".to_owned())
                    })?;
                }
                node_index += if old_generated_count > 0 {
                    old_screen_count.checked_mul(2).ok_or_else(|| {
                        AssetError::InvalidProject(
                            "dialogue continuation node count overflows".to_owned(),
                        )
                    })?
                } else if invocation_template.is_some() {
                    2
                } else {
                    1
                };
                logical_marker_index = logical_marker_index.checked_add(1).ok_or_else(|| {
                    AssetError::InvalidProject("logical marker count overflows".to_owned())
                })?;
            }
        }
    }
    if let Some(marker) = by_marker.keys().next() {
        return Err(AssetError::InvalidProject(format!(
            "scenario-dialogue.json entry {} contains unknown logical marker {}",
            source.entry_id, marker
        )));
    }
    if let Some(text_index) = by_text.keys().next() {
        return Err(AssetError::InvalidProject(format!(
            "scenario-dialogue.json entry {} contains unknown text record {}",
            source.entry_id, text_index
        )));
    }
    let hydrated = ScriptSourceDocument {
        document_version: SOURCE_DOCUMENT_VERSION,
        entry_id: source.entry_id,
        charset: charset_map.id().to_owned(),
        stream_count: source.stream_count,
        nodes,
        secondary_records: secondary_records.to_vec(),
        relocation_model: source.relocation_model.clone(),
    };
    validate_source_text_coverage(&hydrated, charset_map)?;
    Ok(hydrated)
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
    screen_break_after_pages: Vec<u32>,
    screen_speaker_refresh_after_pages: Vec<u32>,
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
    decode_with_charset(
        input,
        asset_directory,
        output_stem,
        entry_id,
        allocation_size,
        charset::default_map(),
    )
}

pub fn decode_with_charset(
    input: &[u8],
    asset_directory: &Path,
    output_stem: &str,
    entry_id: u32,
    allocation_size: usize,
    charset_map: &charset::CharsetMap,
) -> Result<AssetKind, AssetError> {
    decode_with_charset_and_metadata(
        input,
        asset_directory,
        output_stem,
        entry_id,
        allocation_size,
        charset_map,
        None,
    )
}

pub fn decode_with_charset_and_metadata(
    input: &[u8],
    asset_directory: &Path,
    _output_stem: &str,
    entry_id: u32,
    allocation_size: usize,
    charset_map: &charset::CharsetMap,
    engine_metadata: Option<ScMetadata>,
) -> Result<AssetKind, AssetError> {
    let parsed = parse_with_metadata_policy_and_override(
        input,
        entry_id,
        allocation_size,
        true,
        engine_metadata,
    )?;
    let internal_directory = asset_directory.join(".rz-internal").join("sc");
    fs::create_dir_all(&internal_directory)?;
    let editable_file_name = format!("id{entry_id:05}.script-ir.json");
    let editable_name = format!(".rz-internal/sc/{editable_file_name}");
    let document_name = format!(".rz-internal/sc/id{entry_id:05}.script-state.json");

    let mut source_document = build_source_document(
        entry_id,
        &parsed.words,
        &parsed.stream_table,
        &parsed.primary_offsets,
        &parsed.secondary_records,
        charset_map,
    )?;
    validate_source_roundtrip(
        entry_id,
        &parsed.words,
        &parsed.secondary_records,
        &source_document,
        charset_map,
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
        editable: editable_file_name,
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

fn simple_dialogue_invocation_target(node: &ScriptSourceNode) -> Option<u32> {
    let ScriptSourceNode::Raw { words, .. } = node else {
        return None;
    };
    (words.len() == 4 && words[0] == 0xfffb && words[1] == 0xff68)
        .then_some(u32::from(words[2]))
}

fn build_dialogue_entry_from_source(
    source: &ScriptSourceDocument,
    metadata: &BTreeMap<(u32, u32), ScenarioDialogueMetadataItem>,
) -> Result<ScenarioDialogueEntry, AssetError> {
    let dialogue_nodes = source
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(node_index, node)| match node {
            ScriptSourceNode::Dialogue {
                marker_index,
                speaker,
                pages,
                ..
            } => Some((*marker_index, (node_index, speaker, pages))),
            ScriptSourceNode::Raw { .. } | ScriptSourceNode::Text { .. } => None,
        })
        .collect::<BTreeMap<_, _>>();
    let physical_count = u32::try_from(dialogue_nodes.len()).map_err(|_| {
        AssetError::InvalidProject("scenario dialogue count exceeds u32".to_owned())
    })?;
    for marker_index in 0..physical_count {
        if !dialogue_nodes.contains_key(&marker_index) {
            return Err(AssetError::InvalidProject(format!(
                "SC entry {} has a non-contiguous primary marker sequence; missing marker {}",
                source.entry_id, marker_index
            )));
        }
    }

    let entry_metadata = metadata
        .iter()
        .filter_map(|(&(entry_id, marker_index), item)| {
            (entry_id == source.entry_id).then_some((marker_index, item))
        })
        .collect::<BTreeMap<_, _>>();
    let generated_total = entry_metadata.values().try_fold(0u32, |total, item| {
        total.checked_add(item.generated_marker_count).ok_or_else(|| {
            AssetError::InvalidProject(format!(
                "dialogue continuation count overflows for entry {}",
                source.entry_id
            ))
        })
    })?;
    let logical_count = physical_count.checked_sub(generated_total).ok_or_else(|| {
        AssetError::InvalidProject(format!(
            "dialogue metadata for entry {} generates more markers than the archive contains",
            source.entry_id
        ))
    })?;
    if let Some((&marker_index, _)) = entry_metadata.range(logical_count..).next() {
        return Err(AssetError::InvalidProject(format!(
            "dialogue metadata entry {} references logical marker {}, but only {} logical markers remain after folding",
            source.entry_id, marker_index, logical_count
        )));
    }

    let mut dialogues = Vec::with_capacity(usize::try_from(logical_count).unwrap_or(0));
    let mut physical_shift = 0u32;
    for logical_marker in 0..logical_count {
        let item = entry_metadata.get(&logical_marker).copied();
        let generated_marker_count = item
            .map(|value| value.generated_marker_count)
            .unwrap_or(0);
        let physical_root = logical_marker.checked_add(physical_shift).ok_or_else(|| {
            AssetError::InvalidProject("dialogue marker mapping overflows u32".to_owned())
        })?;
        let screen_count = generated_marker_count.checked_add(1).ok_or_else(|| {
            AssetError::InvalidProject("dialogue continuation count overflows".to_owned())
        })?;
        let mut speaker = None::<String>;
        let mut page_texts = Vec::<String>::new();

        for screen_offset in 0..screen_count {
            let physical_marker = physical_root.checked_add(screen_offset).ok_or_else(|| {
                AssetError::InvalidProject("dialogue marker mapping overflows u32".to_owned())
            })?;
            let Some((node_index, screen_speaker, pages)) =
                dialogue_nodes.get(&physical_marker).copied()
            else {
                return Err(AssetError::InvalidProject(format!(
                    "dialogue metadata entry {} marker {} expects physical marker {}, but it is missing",
                    source.entry_id, logical_marker, physical_marker
                )));
            };
            if let Some(expected) = speaker.as_deref() {
                if expected != screen_speaker.as_str() {
                    return Err(AssetError::InvalidProject(format!(
                        "dialogue continuation entry {} marker {} changes speaker at physical marker {}",
                        source.entry_id, logical_marker, physical_marker
                    )));
                }
            } else {
                speaker = Some(screen_speaker.clone());
            }
            if generated_marker_count > 0 {
                let invocation_index = node_index.checked_add(1).ok_or_else(|| {
                    AssetError::InvalidProject("dialogue node index overflows".to_owned())
                })?;
                let target = source
                    .nodes
                    .get(invocation_index)
                    .and_then(simple_dialogue_invocation_target);
                if target != Some(physical_marker) {
                    return Err(AssetError::InvalidProject(format!(
                        "portable dialogue metadata does not match SC entry {}: physical marker {} is not followed by the proven `FFFB FF68 marker arg` invocation",
                        source.entry_id, physical_marker
                    )));
                }
                if screen_offset > 0 {
                    let screen_offset_usize = usize::try_from(screen_offset).map_err(|_| {
                        AssetError::InvalidProject(
                            "dialogue continuation count exceeds usize".to_owned(),
                        )
                    })?;
                    let node_offset = screen_offset_usize.checked_mul(2).ok_or_else(|| {
                        AssetError::InvalidProject(
                            "dialogue continuation node index overflows".to_owned(),
                        )
                    })?;
                    let expected_node_index = dialogue_nodes
                        .get(&physical_root)
                        .map(|(root_index, _, _)| *root_index)
                        .unwrap_or(node_index)
                        .checked_add(node_offset)
                        .ok_or_else(|| {
                            AssetError::InvalidProject("dialogue continuation node index overflows".to_owned())
                        })?;
                    if node_index != expected_node_index {
                        return Err(AssetError::InvalidProject(format!(
                            "portable dialogue metadata does not match SC entry {}: physical marker {} is not an adjacent generated continuation",
                            source.entry_id, physical_marker
                        )));
                    }
                }
            }
            page_texts.extend(pages.iter().map(|page| page.text.clone()));
        }

        let row_joiners = match item {
            Some(item) => {
                let expected = page_texts.len().saturating_sub(1);
                if item.row_joiners.len() != expected {
                    return Err(AssetError::InvalidProject(format!(
                        "dialogue metadata entry {} marker {} contains {} row joiners for {} physical rows; expected {}",
                        source.entry_id,
                        logical_marker,
                        item.row_joiners.len(),
                        page_texts.len(),
                        expected
                    )));
                }
                item.row_joiners.clone()
            }
            None => derive_heuristic_row_joiners(page_texts.iter().map(String::as_str)),
        };
        dialogues.push(ScenarioDialogueItem {
            marker_index: logical_marker,
            speaker: speaker.unwrap_or_default(),
            text: merge_engine_lines_to_user_text_with_joiners(
                page_texts.iter().map(String::as_str),
                &row_joiners,
            ),
            engine_lines_override: None,
            engine_screens_override: None,
            row_joiners_override: Some(row_joiners),
            generated_marker_count_override: Some(generated_marker_count),
        });
        physical_shift = physical_shift
            .checked_add(generated_marker_count)
            .ok_or_else(|| {
                AssetError::InvalidProject("dialogue marker mapping overflows u32".to_owned())
            })?;
    }

    let mut texts = source
        .nodes
        .iter()
        .filter_map(|node| match node {
            ScriptSourceNode::Text {
                text_index,
                grammar,
                text,
                ..
            } => Some(ScenarioTextItem {
                text_index: *text_index,
                grammar: *grammar,
                text: text.clone(),
            }),
            ScriptSourceNode::Raw { .. } | ScriptSourceNode::Dialogue { .. } => None,
        })
        .collect::<Vec<_>>();
    texts.sort_by_key(|item| item.text_index);
    Ok(ScenarioDialogueEntry {
        entry_id: source.entry_id,
        dialogues,
        texts,
    })
}

pub fn write_routing_document(
    asset_directory: &Path,
    assets: &[AssetEntry],
    debug_script_ir: bool,
    charset_map: &charset::CharsetMap,
    portable_dialogue_metadata: Option<&ScenarioDialogueMetadataDocument>,
) -> Result<Vec<u32>, AssetError> {
    let mut documents =
        BTreeMap::<u32, (u32, String, ScriptDocument, ScriptSourceDocument)>::new();
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
        let metadata_path = asset_directory.join(document);
        let metadata: ScriptDocument =
            serde_json::from_slice(&fs::read(&metadata_path)?)?;
        let metadata_directory = metadata_path.parent().unwrap_or(asset_directory);
        let source: ScriptSourceDocument =
            serde_json::from_slice(&fs::read(metadata_directory.join(&metadata.editable))?)?;
        if metadata.entry_id != entry_id || source.entry_id != entry_id {
            return Err(AssetError::InvalidProject(format!(
                "sc.cpk manifest/document entry ID mismatch for {entry_id}"
            )));
        }
        if documents
            .insert(
                entry_id,
                (asset.order, document.clone(), metadata, source),
            )
            .is_some()
        {
            return Err(AssetError::InvalidProject(format!(
                "sc.cpk has duplicate engine ID {entry_id}"
            )));
        }
    }

    let portable_metadata_index = match portable_dialogue_metadata {
        Some(document) => index_dialogue_metadata_document(
            document,
            "portable dialogue metadata companion",
        )?,
        None => BTreeMap::new(),
    };
    for &(entry_id, marker_index) in portable_metadata_index.keys() {
        if !documents.contains_key(&entry_id) {
            return Err(AssetError::InvalidProject(format!(
                "portable dialogue metadata references missing SC entry {entry_id} marker {marker_index}"
            )));
        }
    }

    let mut transitions = Vec::new();
    for (&source_entry_id, (_, _, _, source)) in &documents {
        for (node_index, node) in source.nodes.iter().enumerate() {
            let spans = match node {
                ScriptSourceNode::Raw { words, .. } => vec![(0usize, words.as_slice())],
                ScriptSourceNode::Text {
                    prefix_words,
                    source_glyphs,
                    suffix_words,
                    ..
                } => vec![
                    (0usize, prefix_words.as_slice()),
                    (
                        prefix_words.len() + source_glyphs.len() + 1,
                        suffix_words.as_slice(),
                    ),
                ],
                ScriptSourceNode::Dialogue { .. } => Vec::new(),
            };
            for (base_word_index, words) in spans {
                for word_index in 0..words.len().saturating_sub(2) {
                    if words[word_index] != 0xffef {
                        continue;
                    }
                    let target_entry_id = u32::from(words[word_index + 1]);
                    let target_stream_id = u32::from(words[word_index + 2]);
                    let Some((_, _, _, target)) = documents.get(&target_entry_id) else {
                        continue;
                    };
                    if target_stream_id >= target.stream_count {
                        continue;
                    }
                    transitions.push(ScenarioTransition {
                        source_entry_id,
                        source_node_index: u32::try_from(node_index).unwrap_or(u32::MAX),
                        source_word_index: u32::try_from(base_word_index + word_index)
                            .unwrap_or(u32::MAX),
                        target_entry_id,
                        target_stream_id,
                        opcode: "FFEF".to_owned(),
                    });
                }
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

    let navigation = build_scenario_navigation(&documents, &transitions, 0x56);
    let navigation_rank = navigation
        .iter()
        .map(|placement| (placement.entry_id, placement.navigation_order))
        .collect::<BTreeMap<_, _>>();
    transitions.sort_by_key(|transition| {
        (
            navigation_rank
                .get(&transition.source_entry_id)
                .copied()
                .unwrap_or(u32::MAX),
            transition.source_node_index,
            transition.source_word_index,
            navigation_rank
                .get(&transition.target_entry_id)
                .copied()
                .unwrap_or(u32::MAX),
            transition.target_stream_id,
        )
    });

    let entries = navigation
        .iter()
        .map(|placement| {
            let (archive_order, _, _, source) = documents
                .get(&placement.entry_id)
                .expect("navigation entry must exist in the document map");
            ScenarioEntryIdentity {
                navigation_order: placement.navigation_order,
                component_root_entry_id: placement.component_root_entry_id,
                route_depth: placement.route_depth,
                reachable_from_startup: placement.reachable_from_startup,
                entry_id: placement.entry_id,
                archive_order: *archive_order,
                stream_count: source.stream_count,
            }
        })
        .collect();

    let dialogue_entries = navigation
        .iter()
        .map(|placement| {
            let (_, _, _, source) = documents
                .get(&placement.entry_id)
                .expect("navigation entry must exist in the document map");
            build_dialogue_entry_from_source(source, &portable_metadata_index)
        })
        .collect::<Result<Vec<_>, AssetError>>()?;
    let dialogue_document = ScenarioDialogueDocument {
        document_version: DIALOGUE_DOCUMENT_VERSION,
        archive: "sc.cpk".to_owned(),
        entries: dialogue_entries,
    };
    fs::write(
        asset_directory.join("scenario-dialogue.json"),
        serde_json::to_vec_pretty(&dialogue_document)?,
    )?;
    let dialogue_metadata_entries = dialogue_document
        .entries
        .iter()
        .map(|entry| (entry.entry_id, entry.clone()))
        .collect::<BTreeMap<_, _>>();
    let dialogue_metadata_document =
        build_dialogue_metadata_document(&dialogue_metadata_entries, None);

    let routing_document = ScenarioRoutingDocument {
        document_version: ROUTING_DOCUMENT_VERSION,
        archive: "sc.cpk".to_owned(),
        identity_model: "entry_id is the engine-visible ITOC file ID passed unchanged to FUN_81053CB6; archive_order records CPK iteration/emission order and is not a scenario number"
            .to_owned(),
        chronology_model: "the scenario is a branching directed graph and has no single universal playthrough chronology. scenario-dialogue.json uses deterministic presentation order while entry_id remains the engine-visible identity"
            .to_owned(),
        navigation_model: "dependency-aware topological presentation beginning at startup entry 86. Observed FFEF edges constrain source before target; shared convergence nodes wait for all observed predecessors. Disconnected components are appended by engine-ID root order"
            .to_owned(),
        transition_model: "each item is an FFEF word triple found in a raw node whose target entry and stream are in range. The complete opcode-width/control-flow grammar is not recovered, so this is navigation evidence rather than a guaranteed execution trace"
            .to_owned(),
        dialogue_document: "scenario-dialogue.json".to_owned(),
        startup: ScenarioRouteTarget {
            entry_id: 0x56,
            stream_id: 0,
            evidence: "new-game initialization at 0x8101AD94 calls FUN_8101BEC6 with r0=0x56 and r3=0"
                .to_owned(),
        },
        entries,
        transitions,
    };

    if debug_script_ir {
        let debug_directory = asset_directory.join("debug").join("scenario-ir");
        fs::create_dir_all(&debug_directory)?;
        for placement in &navigation {
            let (_, _, metadata, source) = documents
                .get(&placement.entry_id)
                .expect("navigation entry must exist in the document map");
            let stem = format!(
                "{:05}-id{:05}",
                placement.navigation_order, placement.entry_id
            );
            let source_name = format!("{stem}.script-ir.json");
            let state_name = format!("{stem}.script-state.json");
            let mut debug_metadata = metadata.clone();
            debug_metadata.editable = source_name.clone();
            fs::write(
                debug_directory.join(&source_name),
                serde_json::to_vec_pretty(source)?,
            )?;
            fs::write(
                debug_directory.join(&state_name),
                serde_json::to_vec_pretty(&debug_metadata)?,
            )?;
        }
    }

    let bundle_entries = navigation
        .iter()
        .map(|placement| {
            let (_, _, state, source) = documents
                .get(&placement.entry_id)
                .expect("navigation entry must exist in the document map");
            Ok(ScenarioStateEntry {
                entry_id: placement.entry_id,
                state: compact_script_state(state),
                source: compact_source_document(source, charset_map)?,
            })
        })
        .collect::<Result<Vec<_>, AssetError>>()?;
    let bundle = ScenarioStateBundle {
        document_version: STATE_BUNDLE_VERSION,
        archive: "sc.cpk".to_owned(),
        routing: Some(routing_document),
        dialogue_metadata: Some(dialogue_metadata_document),
        entries: bundle_entries,
    };
    let internal_directory = asset_directory.join(".rz-internal");
    fs::create_dir_all(&internal_directory)?;
    let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(&serde_json::to_vec(&bundle)?)?;
    fs::write(asset_directory.join(STATE_BUNDLE_PATH), encoder.finish()?)?;
    let temporary_entry_directory = internal_directory.join("sc");
    if temporary_entry_directory.exists() {
        fs::remove_dir_all(temporary_entry_directory)?;
    }
    Ok(navigation.iter().map(|item| item.entry_id).collect())
}

fn build_scenario_navigation<T>(
    documents: &BTreeMap<u32, T>,
    transitions: &[ScenarioTransition],
    startup_entry_id: u32,
) -> Vec<ScenarioNavigationPlacement> {
    let mut adjacency = BTreeMap::<u32, Vec<u32>>::new();
    let mut global_indegree = documents
        .keys()
        .copied()
        .map(|entry_id| (entry_id, 0u32))
        .collect::<BTreeMap<_, _>>();

    for transition in transitions {
        let targets = adjacency.entry(transition.source_entry_id).or_default();
        if !targets.contains(&transition.target_entry_id) {
            targets.push(transition.target_entry_id);
            if let Some(value) = global_indegree.get_mut(&transition.target_entry_id) {
                *value = value.saturating_add(1);
            }
        }
    }

    let mut visited = BTreeSet::new();
    let mut output = Vec::with_capacity(documents.len());
    append_scenario_component(
        startup_entry_id,
        true,
        documents,
        &adjacency,
        &mut visited,
        &mut output,
    );

    let disconnected_roots = documents
        .keys()
        .copied()
        .filter(|entry_id| {
            !visited.contains(entry_id)
                && global_indegree.get(entry_id).copied().unwrap_or_default() == 0
        })
        .collect::<Vec<_>>();
    for root_entry_id in disconnected_roots {
        append_scenario_component(
            root_entry_id,
            false,
            documents,
            &adjacency,
            &mut visited,
            &mut output,
        );
    }

    for entry_id in documents.keys().copied() {
        append_scenario_component(
            entry_id,
            false,
            documents,
            &adjacency,
            &mut visited,
            &mut output,
        );
    }
    output
}

fn append_scenario_component<T>(
    root_entry_id: u32,
    reachable_from_startup: bool,
    documents: &BTreeMap<u32, T>,
    adjacency: &BTreeMap<u32, Vec<u32>>,
    visited: &mut BTreeSet<u32>,
    output: &mut Vec<ScenarioNavigationPlacement>,
) {
    if !documents.contains_key(&root_entry_id) || visited.contains(&root_entry_id) {
        return;
    }

    let mut discovery_rank = BTreeMap::<u32, u32>::new();
    let mut route_depth = BTreeMap::<u32, u32>::new();
    let mut queue = VecDeque::new();
    discovery_rank.insert(root_entry_id, 0);
    route_depth.insert(root_entry_id, 0);
    queue.push_back(root_entry_id);

    while let Some(entry_id) = queue.pop_front() {
        let depth = route_depth.get(&entry_id).copied().unwrap_or_default();
        if let Some(targets) = adjacency.get(&entry_id) {
            for target_entry_id in targets {
                if visited.contains(target_entry_id)
                    || !documents.contains_key(target_entry_id)
                    || discovery_rank.contains_key(target_entry_id)
                {
                    continue;
                }
                let rank = u32::try_from(discovery_rank.len()).unwrap_or(u32::MAX);
                discovery_rank.insert(*target_entry_id, rank);
                route_depth.insert(*target_entry_id, depth.saturating_add(1));
                queue.push_back(*target_entry_id);
            }
        }
    }

    let component = discovery_rank.keys().copied().collect::<BTreeSet<_>>();
    let mut indegree = component
        .iter()
        .copied()
        .map(|entry_id| (entry_id, 0u32))
        .collect::<BTreeMap<_, _>>();
    for source_entry_id in &component {
        if let Some(targets) = adjacency.get(source_entry_id) {
            for target_entry_id in targets {
                if component.contains(target_entry_id) {
                    if let Some(value) = indegree.get_mut(target_entry_id) {
                        *value = value.saturating_add(1);
                    }
                }
            }
        }
    }

    let mut emitted = BTreeSet::new();
    while emitted.len() < component.len() {
        let next = component
            .iter()
            .copied()
            .filter(|entry_id| {
                !emitted.contains(entry_id)
                    && indegree.get(entry_id).copied().unwrap_or_default() == 0
            })
            .min_by_key(|entry_id| {
                (
                    discovery_rank.get(entry_id).copied().unwrap_or(u32::MAX),
                    *entry_id,
                )
            })
            .or_else(|| {
                // A cycle would mean the candidate FFEF graph is not a DAG.
                // Keep output deterministic without claiming a proven runtime loop.
                component
                    .iter()
                    .copied()
                    .filter(|entry_id| !emitted.contains(entry_id))
                    .min_by_key(|entry_id| {
                        (
                            discovery_rank.get(entry_id).copied().unwrap_or(u32::MAX),
                            *entry_id,
                        )
                    })
            });
        let Some(entry_id) = next else {
            break;
        };
        emitted.insert(entry_id);
        visited.insert(entry_id);
        output.push(ScenarioNavigationPlacement {
            navigation_order: u32::try_from(output.len()).unwrap_or(u32::MAX),
            component_root_entry_id: root_entry_id,
            route_depth: route_depth.get(&entry_id).copied().unwrap_or_default(),
            reachable_from_startup,
            entry_id,
        });
        if let Some(targets) = adjacency.get(&entry_id) {
            for target_entry_id in targets {
                if let Some(value) = indegree.get_mut(target_entry_id) {
                    *value = value.saturating_sub(1);
                }
            }
        }
    }
}

pub fn apply_presentation_order(
    assets: &mut Vec<AssetEntry>,
    presentation_order: &[u32],
) -> Result<(), AssetError> {
    if presentation_order.len() != assets.len() {
        return Err(AssetError::InvalidProject(format!(
            "scenario presentation order contains {} entries, project contains {}",
            presentation_order.len(),
            assets.len()
        )));
    }

    let mut by_id = BTreeMap::<u32, AssetEntry>::new();
    for asset in assets.drain(..) {
        let entry_id = asset.id.ok_or_else(|| {
            AssetError::InvalidProject(format!(
                "sc.cpk entry {} has no ITOC engine ID",
                asset.file_name
            ))
        })?;
        if by_id.insert(entry_id, asset).is_some() {
            return Err(AssetError::InvalidProject(format!(
                "sc.cpk has duplicate engine ID {entry_id}"
            )));
        }
    }

    let mut reordered = Vec::with_capacity(presentation_order.len());
    for entry_id in presentation_order.iter().copied() {
        let mut asset = by_id.remove(&entry_id).ok_or_else(|| {
            AssetError::InvalidProject(format!(
                "scenario presentation order references missing engine ID {entry_id}"
            ))
        })?;
        let AssetKind::Script { document } = &mut asset.kind else {
            return Err(AssetError::InvalidProject(format!(
                "scenario entry {entry_id} is not a script asset"
            )));
        };
        *document = STATE_BUNDLE_PATH.to_owned();
        reordered.push(asset);
    }

    if !by_id.is_empty() {
        return Err(AssetError::InvalidProject(
            "scenario presentation order omitted one or more engine IDs".to_owned(),
        ));
    }
    *assets = reordered;
    Ok(())
}

fn read_state_bundle_document(project_directory: &Path) -> Result<ScenarioStateBundle, AssetError> {
    let preferred = project_directory.join(STATE_BUNDLE_PATH);
    let path = if preferred.exists() {
        preferred
    } else {
        project_directory.join(LEGACY_STATE_BUNDLE_PATH)
    };
    let mut decoder = GzDecoder::new(fs::File::open(&path)?);
    let mut encoded = Vec::new();
    decoder.read_to_end(&mut encoded)?;
    let bundle: ScenarioStateBundle = serde_json::from_slice(&encoded)?;
    let version_ok = bundle.document_version == STATE_BUNDLE_VERSION;
    if !version_ok || !bundle.archive.eq_ignore_ascii_case("sc.cpk") {
        return Err(AssetError::InvalidProject(format!(
            "{} has an unsupported version or archive identity; re-extract sc.cpk with this build",
            path.display()
        )));
    }
    Ok(bundle)
}

pub fn load_state_bundle(
    project_directory: &Path,
) -> Result<BTreeMap<u32, ScenarioStateEntry>, AssetError> {
    let bundle = read_state_bundle_document(project_directory)?;
    let mut entries = BTreeMap::new();
    for entry in bundle.entries {
        if entry.entry_id != entry.state.entry_id || entry.entry_id != entry.source.entry_id {
            return Err(AssetError::InvalidProject(
                "SC build state bundle contains an entry ID mismatch".to_owned(),
            ));
        }
        validate_machine_state(&entry.state)?;
        validate_machine_document(&entry.source)?;
        if entries.insert(entry.entry_id, entry).is_some() {
            return Err(AssetError::InvalidProject(
                "SC build state bundle contains a duplicate entry_id".to_owned(),
            ));
        }
    }
    Ok(entries)
}

pub fn load_dialogue_document(
    project_directory: &Path,
) -> Result<BTreeMap<u32, ScenarioDialogueEntry>, AssetError> {
    let path = project_directory.join("scenario-dialogue.json");
    let document: ScenarioDialogueDocument = serde_json::from_slice(&fs::read(&path)?)?;
    if document.document_version != DIALOGUE_DOCUMENT_VERSION
        || !document.archive.eq_ignore_ascii_case("sc.cpk")
    {
        return Err(AssetError::InvalidProject(
            "scenario-dialogue.json has an unsupported version or archive identity".to_owned(),
        ));
    }
    let mut entries = BTreeMap::new();
    for entry in document.entries {
        let mut markers = BTreeSet::new();
        for dialogue in &entry.dialogues {
            if !markers.insert(dialogue.marker_index) {
                return Err(AssetError::InvalidProject(format!(
                    "scenario-dialogue.json entry {} contains duplicate marker {}",
                    entry.entry_id, dialogue.marker_index
                )));
            }
            let engine_lines = dialogue.engine_lines();
            if engine_lines.iter().all(|line| line.is_empty()) {
                return Err(AssetError::InvalidProject(format!(
                    "scenario-dialogue.json entry {} marker {} has no text",
                    entry.entry_id, dialogue.marker_index
                )));
            }
        }
        let mut text_indexes = BTreeSet::new();
        for text in &entry.texts {
            if !text_indexes.insert(text.text_index) {
                return Err(AssetError::InvalidProject(format!(
                    "scenario-dialogue.json entry {} contains duplicate text_index {}",
                    entry.entry_id, text.text_index
                )));
            }
            if text.text.is_empty() {
                return Err(AssetError::InvalidProject(format!(
                    "scenario-dialogue.json entry {} text record {} is empty; the proven non-dialogue grammars require at least one glyph",
                    entry.entry_id, text.text_index
                )));
            }
        }
        if entries.insert(entry.entry_id, entry).is_some() {
            return Err(AssetError::InvalidProject(
                "scenario-dialogue.json contains a duplicate entry_id".to_owned(),
            ));
        }
    }
    let metadata = load_dialogue_metadata_document(project_directory)?;
    for ((entry_id, marker_index), item) in metadata {
        let entry = entries.get_mut(&entry_id).ok_or_else(|| {
            AssetError::InvalidProject(format!(
                "internal SC dialogue metadata references missing entry {}",
                entry_id
            ))
        })?;
        let dialogue = entry
            .dialogues
            .iter_mut()
            .find(|dialogue| dialogue.marker_index == marker_index)
            .ok_or_else(|| {
                AssetError::InvalidProject(format!(
                    "internal SC dialogue metadata references missing entry {} marker {}",
                    entry_id, marker_index
                ))
            })?;
        dialogue.set_roundtrip_metadata_for_build(
            item.row_joiners,
            item.generated_marker_count,
        );
    }
    Ok(entries)
}

pub fn dialogue_metadata_companion_path(archive_path: &Path) -> PathBuf {
    let parent = archive_path.parent().unwrap_or_else(|| Path::new("."));
    let name = archive_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("sc.cpk");
    parent.join(format!("{name}{PORTABLE_DIALOGUE_METADATA_SUFFIX}"))
}

pub fn read_dialogue_metadata_file(
    path: &Path,
) -> Result<ScenarioDialogueMetadataDocument, AssetError> {
    let document: ScenarioDialogueMetadataDocument =
        serde_json::from_slice(&fs::read(path)?)?;
    let _ = index_dialogue_metadata_document(&document, &path.display().to_string())?;
    Ok(document)
}

pub fn load_portable_dialogue_metadata(
    archive_path: &Path,
) -> Result<Option<ScenarioDialogueMetadataDocument>, AssetError> {
    let path = dialogue_metadata_companion_path(archive_path);
    if !path.exists() {
        return Ok(None);
    }
    let document = read_dialogue_metadata_file(&path)?;
    if document.archive_sha256.is_none() {
        return Err(AssetError::InvalidProject(format!(
            "portable dialogue metadata companion {} omits archive_sha256",
            path.display()
        )));
    }
    Ok(Some(document))
}

pub fn dialogue_metadata_has_entries(document: &ScenarioDialogueMetadataDocument) -> bool {
    document.entries.iter().any(|entry| !entry.dialogues.is_empty())
}

fn load_dialogue_metadata_document(
    project_directory: &Path,
) -> Result<BTreeMap<(u32, u32), ScenarioDialogueMetadataItem>, AssetError> {
    let bundle = read_state_bundle_document(project_directory)?;
    if let Some(document) = bundle.dialogue_metadata.as_ref() {
        return index_dialogue_metadata_document(
            document,
            ".rz-internal/sc-build-state.json.gz dialogue_metadata",
        );
    }
    let legacy_path = project_directory.join(LEGACY_DIALOGUE_METADATA_PATH);
    if !legacy_path.exists() {
        return Ok(BTreeMap::new());
    }
    let document = read_dialogue_metadata_file(&legacy_path)?;
    index_dialogue_metadata_document(&document, LEGACY_DIALOGUE_METADATA_PATH)
}

fn index_dialogue_metadata_document(
    document: &ScenarioDialogueMetadataDocument,
    label: &str,
) -> Result<BTreeMap<(u32, u32), ScenarioDialogueMetadataItem>, AssetError> {
    if document.document_version != DIALOGUE_DOCUMENT_VERSION
        || !document.archive.eq_ignore_ascii_case("sc.cpk")
    {
        return Err(AssetError::InvalidProject(
            format!("{label} has an unsupported version or archive identity"),
        ));
    }
    if let Some(digest) = document.archive_sha256.as_deref() {
        if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(AssetError::InvalidProject(format!(
                "{label} contains an invalid archive_sha256"
            )));
        }
    }
    let mut out = BTreeMap::new();
    for entry in &document.entries {
        for item in &entry.dialogues {
            if item.row_joiners.iter().any(|joiner| joiner != " " && !joiner.is_empty()) {
                return Err(AssetError::InvalidProject(format!(
                    "{label} entry {} marker {} contains an unsupported row joiner; only empty string and single ASCII space are allowed",
                    entry.entry_id, item.marker_index
                )));
            }
            let marker_index = item.marker_index;
            if out
                .insert((entry.entry_id, marker_index), item.clone())
                .is_some()
            {
                return Err(AssetError::InvalidProject(format!(
                    "{label} contains duplicate entry {} marker {}",
                    entry.entry_id, marker_index
                )));
            }
        }
    }
    Ok(out)
}

pub fn build_dialogue_metadata_document(
    entries: &BTreeMap<u32, ScenarioDialogueEntry>,
    archive_sha256: Option<String>,
) -> ScenarioDialogueMetadataDocument {
    let mut metadata_entries = Vec::<ScenarioDialogueMetadataEntry>::new();
    for (entry_id, entry) in entries {
        let mut dialogues = Vec::<ScenarioDialogueMetadataItem>::new();
        for dialogue in &entry.dialogues {
            let mut joiners = dialogue.row_joiners_for_build();
            let generated_marker_count = dialogue.generated_marker_count_for_build();
            let has_semantic_joiner = joiners.iter().any(|joiner| !joiner.is_empty());
            if generated_marker_count == 0 && !has_semantic_joiner {
                joiners.clear();
            }
            if joiners.is_empty() && generated_marker_count == 0 {
                continue;
            }
            dialogues.push(ScenarioDialogueMetadataItem {
                marker_index: dialogue.marker_index,
                row_joiners: joiners,
                generated_marker_count,
            });
        }
        if !dialogues.is_empty() {
            metadata_entries.push(ScenarioDialogueMetadataEntry {
                entry_id: *entry_id,
                dialogues,
            });
        }
    }
    ScenarioDialogueMetadataDocument {
        document_version: DIALOGUE_DOCUMENT_VERSION,
        archive: "sc.cpk".to_owned(),
        archive_sha256,
        entries: metadata_entries,
    }
}

fn apply_dialogue_entry(
    source: &mut ScriptSourceDocument,
    edited: &ScenarioDialogueEntry,
    charset_map: &charset::CharsetMap,
) -> Result<(), AssetError> {
    if source.entry_id != edited.entry_id {
        return Err(AssetError::InvalidProject(format!(
            "scenario dialogue entry {} does not match internal script entry {}",
            edited.entry_id, source.entry_id
        )));
    }
    let mut by_marker = edited
        .dialogues
        .iter()
        .map(|dialogue| (dialogue.marker_index, dialogue))
        .collect::<BTreeMap<_, _>>();
    let mut by_text = edited
        .texts
        .iter()
        .map(|item| (item.text_index, item))
        .collect::<BTreeMap<_, _>>();
    let expected_count = source
        .nodes
        .iter()
        .filter(|node| matches!(node, ScriptSourceNode::Dialogue { .. }))
        .count();
    let expected_text_count = source
        .nodes
        .iter()
        .filter(|node| matches!(node, ScriptSourceNode::Text { .. }))
        .count();
    if by_marker.len() != expected_count {
        return Err(AssetError::InvalidProject(format!(
            "scenario-dialogue.json entry {} contains {} dialogue markers, expected {}",
            source.entry_id,
            by_marker.len(),
            expected_count
        )));
    }
    if by_text.len() != expected_text_count {
        return Err(AssetError::InvalidProject(format!(
            "scenario-dialogue.json entry {} contains {} text records, expected {}",
            source.entry_id,
            by_text.len(),
            expected_text_count
        )));
    }

    let original_nodes = std::mem::take(&mut source.nodes);
    let mut rebuilt_nodes = Vec::<ScriptSourceNode>::with_capacity(original_nodes.len());
    let mut next_marker_index = 0u32;
    let mut marker_remap = BTreeMap::<u32, u32>::new();
    let mut node_iter = original_nodes.into_iter().peekable();
    while let Some(node) = node_iter.next() {
        match node {
            ScriptSourceNode::Text {
                labels,
                text_index,
                grammar,
                prefix_words,
                mut text,
                mut source_glyphs,
                suffix_words,
            } => {
                let edit = by_text.remove(&text_index).ok_or_else(|| {
                    AssetError::InvalidProject(format!(
                        "scenario-dialogue.json entry {} omits text record {}",
                        source.entry_id, text_index
                    ))
                })?;
                if edit.grammar != grammar {
                    return Err(AssetError::InvalidProject(format!(
                        "scenario-dialogue.json entry {} text record {} changes grammar from {:?} to {:?}",
                        source.entry_id, text_index, grammar, edit.grammar
                    )));
                }
                if text.as_str() != edit.text.as_str() {
                    text = edit.text.clone();
                    source_glyphs.clear();
                }
                rebuilt_nodes.push(ScriptSourceNode::Text {
                    labels,
                    text_index,
                    grammar,
                    prefix_words,
                    text,
                    source_glyphs,
                    suffix_words,
                });
            }
            ScriptSourceNode::Dialogue {
                labels,
                marker_index,
                mut speaker,
                mut speaker_source_glyphs,
                pages,
                screen_break_after_pages: preserved_screen_breaks,
                screen_speaker_refresh_after_pages: preserved_speaker_refreshes,
            } => {
                let edit = by_marker.remove(&marker_index).ok_or_else(|| {
                    AssetError::InvalidProject(format!(
                        "scenario-dialogue.json entry {} omits marker {}",
                        source.entry_id, marker_index
                    ))
                })?;
                if speaker.as_str() != edit.speaker.as_str() {
                    speaker = edit.speaker.clone();
                    speaker_source_glyphs.clear();
                }
                let first_marker_index = next_marker_index;
                marker_remap.insert(marker_index, first_marker_index);
                if source_dialogue_matches_edit(edit, &speaker, &pages) {
                    rebuilt_nodes.push(ScriptSourceNode::Dialogue {
                        labels,
                        marker_index: first_marker_index,
                        speaker,
                        speaker_source_glyphs,
                        pages,
                        screen_break_after_pages: preserved_screen_breaks,
                        screen_speaker_refresh_after_pages: preserved_speaker_refreshes,
                    });
                    next_marker_index = next_marker_index.checked_add(1).ok_or_else(|| {
                        AssetError::InvalidProject("dialogue marker count overflows".to_owned())
                    })?;
                    continue;
                }

                let screens = edit.engine_screens_for_build();
                let continuation_invocation = if screens.len() > 1 {
                    let Some(ScriptSourceNode::Raw { labels: raw_labels, words }) = node_iter.peek() else {
                        return Err(AssetError::InvalidProject(format!(
                            "dialogue marker {marker_index} wraps beyond one message entry, but no following VM invocation node is available"
                        )));
                    };
                    Some((raw_labels.clone(), words.clone()))
                } else {
                    None
                };

                for (screen_index, screen_rows) in screens.iter().enumerate() {
                    let current_marker = next_marker_index;
                    let mut rebuilt_pages = Vec::with_capacity(screen_rows.len());
                    for (row_index, text) in screen_rows.iter().enumerate() {
                        let source_glyphs = if screen_index == 0 {
                            pages
                                .get(row_index)
                                .filter(|page| page.text.as_str() == text.as_str())
                                .map(|page| page.source_glyphs.clone())
                                .unwrap_or_default()
                        } else {
                            Vec::new()
                        };
                        rebuilt_pages.push(ScriptDialoguePage {
                            text: text.clone(),
                            source_glyphs,
                        });
                    }
                    rebuilt_nodes.push(ScriptSourceNode::Dialogue {
                        labels: if screen_index == 0 { labels.clone() } else { Vec::new() },
                        marker_index: current_marker,
                        speaker: speaker.clone(),
                        speaker_source_glyphs: speaker_source_glyphs.clone(),
                        pages: rebuilt_pages,
                        screen_break_after_pages: if screen_index == 0 && screens.len() == 1 {
                            preserved_screen_breaks.clone()
                        } else {
                            Vec::new()
                        },
                        screen_speaker_refresh_after_pages: if screen_index == 0 && screens.len() == 1 {
                            preserved_speaker_refreshes.clone()
                        } else {
                            Vec::new()
                        },
                    });
                    if let Some((raw_labels, words)) = continuation_invocation.as_ref() {
                        rebuilt_nodes.push(ScriptSourceNode::Raw {
                            labels: if screen_index == 0 { raw_labels.clone() } else { Vec::new() },
                            words: clone_simple_dialogue_invocation(
                                words,
                                marker_index,
                                current_marker,
                            )?,
                        });
                    }
                    next_marker_index = next_marker_index.checked_add(1).ok_or_else(|| {
                        AssetError::InvalidProject("dialogue marker count overflows".to_owned())
                    })?;
                }
                if continuation_invocation.is_some() {
                    let _ = node_iter.next();
                }
            }
            ScriptSourceNode::Raw { labels, mut words } => {
                remap_dialogue_invocation_words(&mut words, &marker_remap)?;
                rebuilt_nodes.push(ScriptSourceNode::Raw { labels, words });
            }
        }
    }
    source.nodes = rebuilt_nodes;
    if let Some(marker) = by_marker.keys().next() {
        return Err(AssetError::InvalidProject(format!(
            "scenario-dialogue.json entry {} contains unknown marker {}",
            source.entry_id, marker
        )));
    }
    if let Some(text_index) = by_text.keys().next() {
        return Err(AssetError::InvalidProject(format!(
            "scenario-dialogue.json entry {} contains unknown text record {}",
            source.entry_id, text_index
        )));
    }
    validate_source_text_coverage(source, charset_map)?;
    Ok(())
}
fn load_source_for_build(
    path: &Path,
    document: &ScriptDocument,
    dialogue: Option<&ScenarioDialogueEntry>,
    charset_map: &charset::CharsetMap,
) -> Result<ScriptSourceDocument, AssetError> {
    let root = path.parent().unwrap_or_else(|| Path::new("."));
    let mut source: ScriptSourceDocument =
        serde_json::from_slice(&fs::read(root.join(&document.editable))?)?;
    source.secondary_records = document.secondary_records.clone();
    // Allow stock-extracted SC projects to be rebuilt with an expanded charset
    // whose first stock glyph IDs remain compatible. Text coverage and glyph
    // encoding are validated against the build-time charset_map below.
    source.charset = charset_map.id().to_owned();
    if let Some(dialogue) = dialogue {
        apply_dialogue_entry(&mut source, dialogue, charset_map)?;
    }
    Ok(source)
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
    inspect_build_with_charset_and_dialogue(path, charset_map, None)
}

pub fn inspect_build_with_charset_and_dialogue(
    path: &Path,
    charset_map: &charset::CharsetMap,
    dialogue: Option<&ScenarioDialogueEntry>,
) -> Result<ScriptBuildInfo, AssetError> {
    let document: ScriptDocument = serde_json::from_slice(&fs::read(path)?)?;
    validate_document_version(&document)?;
    let source_document = load_source_for_build(path, &document, dialogue, charset_map)?;
    inspect_document_source(&document, &source_document, charset_map)
}

pub fn inspect_state_build_with_charset_and_dialogue(
    state: &ScenarioStateEntry,
    charset_map: &charset::CharsetMap,
    dialogue: Option<&ScenarioDialogueEntry>,
) -> Result<ScriptBuildInfo, AssetError> {
    validate_machine_state(&state.state)?;
    let document = hydrate_script_state(&state.state);
    let dialogue = dialogue.ok_or_else(|| {
        AssetError::InvalidProject(
            "SC compact state requires scenario-dialogue.json during build".to_owned(),
        )
    })?;
    let source = hydrate_machine_source(
        &state.source,
        dialogue,
        &state.state.secondary_records,
        charset_map,
    )?;
    inspect_document_source(&document, &source, charset_map)
}

fn inspect_document_source(
    document: &ScriptDocument,
    source_document: &ScriptSourceDocument,
    charset_map: &charset::CharsetMap,
) -> Result<ScriptBuildInfo, AssetError> {
    validate_source_text_coverage(source_document, charset_map)?;
    let (words, secondary) =
        assemble_source_document(document.entry_id, source_document, charset_map)?;
    let stream_table = parse_offset_table(&words_to_bytes(&words)).map_err(|error| {
        AssetError::InvalidProject(format!("relocated script payload is invalid: {error}"))
    })?;
    let primary = derive_primary_offsets(&words)?;
    let text_ranges = parse_text_ranges(&words, &primary, charset_map.glyph_count())?;
    validate_text_coverage(
        &words,
        &text_ranges,
        usize::try_from(stream_table.table_bytes).unwrap_or(0) / 2,
    )?;
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
        .and_then(|value| value.checked_add(SC_INTEGRITY_FOOTER_SIZE))
        .ok_or_else(|| {
            AssetError::InvalidProject("script allocation requirement overflows".to_owned())
        })?;
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
    encode_with_allocation_and_charset_and_dialogue(
        path,
        allocation_size,
        allow_engine_metadata_change,
        charset_map,
        None,
    )
}

pub fn encode_with_allocation_and_charset_and_dialogue(
    path: &Path,
    allocation_size: usize,
    allow_engine_metadata_change: bool,
    charset_map: &charset::CharsetMap,
    dialogue: Option<&ScenarioDialogueEntry>,
) -> Result<Vec<u8>, AssetError> {
    let document: ScriptDocument = serde_json::from_slice(&fs::read(path)?)?;
    validate_document_version(&document)?;
    let source_document = load_source_for_build(path, &document, dialogue, charset_map)?;
    encode_document_source(
        &document,
        &source_document,
        allocation_size,
        allow_engine_metadata_change,
        charset_map,
    )
}

pub fn encode_state_with_allocation_and_charset_and_dialogue(
    state: &ScenarioStateEntry,
    allocation_size: usize,
    allow_engine_metadata_change: bool,
    charset_map: &charset::CharsetMap,
    dialogue: Option<&ScenarioDialogueEntry>,
) -> Result<Vec<u8>, AssetError> {
    validate_machine_state(&state.state)?;
    let document = hydrate_script_state(&state.state);
    let dialogue = dialogue.ok_or_else(|| {
        AssetError::InvalidProject(
            "SC compact state requires scenario-dialogue.json during build".to_owned(),
        )
    })?;
    let source = hydrate_machine_source(
        &state.source,
        dialogue,
        &state.state.secondary_records,
        charset_map,
    )?;
    encode_document_source(
        &document,
        &source,
        allocation_size,
        allow_engine_metadata_change,
        charset_map,
    )
}

fn encode_document_source(
    document: &ScriptDocument,
    source_document: &ScriptSourceDocument,
    allocation_size: usize,
    allow_engine_metadata_change: bool,
    charset_map: &charset::CharsetMap,
) -> Result<Vec<u8>, AssetError> {

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
    let extracted_footer = decode_hex(&document.opaque_footer_hex)?;
    if extracted_footer.len() != SC_INTEGRITY_FOOTER_SIZE {
        return Err(AssetError::InvalidProject(format!(
            "script integrity footer has {} bytes, expected {SC_INTEGRITY_FOOTER_SIZE}",
            extracted_footer.len()
        )));
    }
    let original_capacity = usize::try_from(document.payload_capacity_bytes).map_err(|_| {
        AssetError::InvalidProject("script payload capacity exceeds usize".to_owned())
    })?;
    let documented_capacity = usize::try_from(document.allocation_size)
        .ok()
        .and_then(|value| value.checked_sub(SCRIPT_BASE + SC_INTEGRITY_FOOTER_SIZE))
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
    validate_source_text_coverage(source_document, charset_map)?;
    let (words, relocated_secondary) =
        assemble_source_document(document.entry_id, source_document, charset_map)?;
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
    let text_ranges = parse_text_ranges(&words, &primary_offsets, charset_map.glyph_count())?;
    validate_text_coverage(&words, &text_ranges, usize::try_from(stream_table.table_bytes).unwrap_or(0) / 2)?;
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
    let footer_start = allocation_size - SC_INTEGRITY_FOOTER_SIZE;
    if output.len() > footer_start {
        return Err(AssetError::InvalidProject(format!(
            "rebuilt logical script ends at {:#x}, exceeding selected payload capacity {footer_start:#x}",
            output.len()
        )));
    }
    output.resize(footer_start, 0);
    let footer = compute_sc_integrity_footer(&output).map_err(|error| {
        AssetError::InvalidProject(format!("failed to generate SC integrity footer: {error}"))
    })?;
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

fn compute_sc_integrity_footer(input: &[u8]) -> Result<[u8; SC_INTEGRITY_FOOTER_SIZE], String> {
    if input.len() % 0x10 != 0 {
        return Err(format!(
            "SC integrity checksum input has {:#x} bytes, expected a multiple of 0x10",
            input.len()
        ));
    }

    // FUN_8102B4AC treats each 16-byte block as two little-endian u64 lanes.
    // Both wrapping sums start at 0x1111111111111111. The stored footer is
    // lane 0 followed by lane 1, both little-endian.
    let mut lane_0 = SC_INTEGRITY_SEED;
    let mut lane_1 = SC_INTEGRITY_SEED;
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

    let mut footer = [0u8; SC_INTEGRITY_FOOTER_SIZE];
    footer[..8].copy_from_slice(&lane_0.to_le_bytes());
    footer[8..].copy_from_slice(&lane_1.to_le_bytes());
    Ok(footer)
}

fn parse_with_metadata_policy(
    input: &[u8],
    entry_id: u32,
    allocation_size: usize,
    validate_engine_metadata: bool,
) -> Result<ParsedScript, AssetError> {
    parse_with_metadata_policy_and_override(
        input,
        entry_id,
        allocation_size,
        validate_engine_metadata,
        None,
    )
}

fn parse_with_metadata_policy_and_override(
    input: &[u8],
    entry_id: u32,
    allocation_size: usize,
    validate_engine_metadata: bool,
    engine_metadata: Option<ScMetadata>,
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
    if working.len() < SC_INTEGRITY_FOOTER_SIZE {
        return Err(AssetError::InvalidFormat(
            "script allocation is shorter than its 16-byte integrity footer".to_owned(),
        ));
    }
    let integrity_offset = working.len() - SC_INTEGRITY_FOOTER_SIZE;
    let expected_footer = compute_sc_integrity_footer(&working[..integrity_offset])
        .map_err(|error| AssetError::InvalidFormat(format!("invalid SC integrity domain: {error}")))?;
    let actual_footer = &working[integrity_offset..];
    if actual_footer != expected_footer.as_slice() {
        return Err(AssetError::InvalidFormat(format!(
            "SC integrity footer mismatch: stored {}, expected {}",
            encode_hex(actual_footer),
            encode_hex(&expected_footer)
        )));
    }

    let voice_header = parse_voice_header(&working[..VOICE_HEADER_SIZE])?;
    let allocated_script = &working[SCRIPT_BASE..];
    if allocated_script.len() <= SC_INTEGRITY_FOOTER_SIZE {
        return Err(AssetError::InvalidFormat(
            "script payload is shorter than its 16-byte integrity footer".to_owned(),
        ));
    }
    let footer_start = allocated_script.len() - SC_INTEGRITY_FOOTER_SIZE;
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
    let metadata = engine_metadata.or_else(|| engine_allocations::sc_metadata(entry_id));
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
    glyph_limit: usize,
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
            .any(|value| usize::from(*value) >= glyph_limit)
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
            .any(|value| usize::from(*value) >= glyph_limit)
        {
            return Err(AssetError::InvalidFormat(format!(
                "primary marker {expected_index} first page contains a non-glyph word"
            )));
        }

        let mut pages = vec![ScriptPageRange {
            start: first_page_start,
            end: first_page_end,
        }];
        let mut screen_break_after_pages = Vec::<u32>::new();
        let mut screen_speaker_refresh_after_pages = Vec::<u32>::new();
        let mut cursor = first_page_end + 1;
        loop {
            if cursor < words.len()
                && words[cursor] == 0xfffb
                && cursor + 1 < words.len()
                && (usize::from(words[cursor + 1]) < glyph_limit
                    || words[cursor + 1] == 0xffff)
            {
                let completed_pages = u32::try_from(pages.len()).map_err(|_| {
                    AssetError::InvalidFormat("script dialogue page count exceeds u32".to_owned())
                })?;
                screen_break_after_pages.push(completed_pages);
                cursor += 1;
                // Speaker refresh handling: generated VWF continuations repeat the
                // original speaker after FFFB so the engine redraws the nameplate
                // on screen 2+.  Treat that repeated span as control metadata, not
                // user text.  Stock or blank-speaker FFFB continuations without a
                // refresh remain accepted by falling through to the existing page
                // parser.
                if speaker_end > speaker_start {
                    let speaker_len = speaker_end - speaker_start;
                    let refresh_end = cursor.saturating_add(speaker_len);
                    if refresh_end < words.len()
                        && words[cursor..refresh_end] == words[speaker_start..speaker_end]
                        && words[refresh_end] == 0xffff
                    {
                        screen_speaker_refresh_after_pages.push(completed_pages);
                        cursor = refresh_end + 1;
                    }
                }
            }
            let page_start = cursor;
            while cursor < words.len() && usize::from(words[cursor]) < glyph_limit {
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
            screen_break_after_pages,
            screen_speaker_refresh_after_pages,
            end: cursor,
        });
    }
    Ok(ranges)
}

fn validate_text_coverage(
    words: &[u16],
    ranges: &[ScriptTextRange],
    body_start: usize,
) -> Result<(), AssetError> {
    let mut parsed_terminators = BTreeSet::new();
    let mut previous_end = 0usize;
    for range in ranges {
        let start = usize::try_from(range.byte_offset)
            .map_err(|_| AssetError::InvalidFormat("script text offset exceeds usize".to_owned()))?
            / 2;
        if start < previous_end || range.end <= start || range.end > words.len() {
            return Err(AssetError::InvalidFormat(format!(
                "primary dialogue {} overlaps another range or exceeds the payload",
                range.marker_index
            )));
        }
        previous_end = range.end;
        for page in &range.pages {
            if page.start > page.end
                || page.end >= words.len()
                || words[page.end] != 0xfffe
                || !parsed_terminators.insert(page.end)
            {
                return Err(AssetError::InvalidFormat(format!(
                    "primary dialogue {} has an invalid or duplicate FFFE page boundary",
                    range.marker_index
                )));
            }
        }
    }

    let all_terminators = words
        .iter()
        .enumerate()
        .skip(body_start)
        .filter_map(|(index, word)| (*word == 0xfffe).then_some(index))
        .collect::<BTreeSet<_>>();
    if all_terminators != parsed_terminators {
        let residual = all_terminators
            .difference(&parsed_terminators)
            .next()
            .copied()
            .or_else(|| parsed_terminators.difference(&all_terminators).next().copied())
            .unwrap_or_default();
        return Err(AssetError::InvalidFormat(format!(
            "script text grammar left an unparsed or inconsistent FFFE page terminator at word {residual:#x}; refusing extraction to avoid hidden text loss"
        )));
    }
    Ok(())
}

fn validate_source_roundtrip(
    entry_id: u32,
    original_words: &[u16],
    original_secondary: &[ScriptSecondaryRecord],
    source: &ScriptSourceDocument,
    charset_map: &charset::CharsetMap,
) -> Result<(), AssetError> {
    let (rebuilt_words, rebuilt_secondary) =
        assemble_source_document(entry_id, source, charset_map)?;
    if rebuilt_words != original_words {
        let mismatch = rebuilt_words
            .iter()
            .zip(original_words)
            .position(|(left, right)| left != right)
            .unwrap_or_else(|| rebuilt_words.len().min(original_words.len()));
        return Err(AssetError::InvalidFormat(format!(
            "script source IR is not byte-exact at word {mismatch:#x}; refusing extraction because the recovered grammar is incomplete"
        )));
    }
    if rebuilt_secondary != original_secondary {
        return Err(AssetError::InvalidFormat(
            "script source IR does not reproduce the secondary relocation table exactly"
                .to_owned(),
        ));
    }
    Ok(())
}

fn glyph_string_end(words: &[u16], start: usize, glyph_limit: usize) -> Option<usize> {
    let mut cursor = start;
    while cursor < words.len() && usize::from(words[cursor]) < glyph_limit {
        cursor += 1;
    }
    (cursor > start && cursor < words.len() && words[cursor] == 0xffff).then_some(cursor)
}

fn inline_text_at(
    words: &[u16],
    opcode_index: usize,
    glyph_limit: usize,
) -> Option<(ScriptTextGrammar, usize, usize)> {
    let opcode = *words.get(opcode_index)?;
    let grammar = match opcode {
        0xff42 => ScriptTextGrammar::InlineFf42,
        0xff8c => ScriptTextGrammar::InlineFf8c,
        _ => return None,
    };
    let text_start = opcode_index.checked_add(1)?;
    if opcode == 0xff8c
        && words.get(text_start).copied() == Some(0)
        && words.get(text_start + 1).copied() == Some(0xffff)
    {
        return None;
    }
    let terminator = glyph_string_end(words, text_start, glyph_limit)?;
    Some((grammar, text_start, terminator))
}

fn push_text_node(
    nodes: &mut Vec<ScriptSourceNode>,
    labels: Vec<String>,
    text_index: &mut u32,
    grammar: ScriptTextGrammar,
    prefix_words: Vec<u16>,
    source_glyphs: Vec<u16>,
    suffix_words: Vec<u16>,
    charset_map: &charset::CharsetMap,
) -> Result<(), AssetError> {
    let text = charset_map.decode_slice(&source_glyphs)?;
    nodes.push(ScriptSourceNode::Text {
        labels,
        text_index: *text_index,
        grammar,
        prefix_words,
        text,
        source_glyphs,
        suffix_words,
    });
    *text_index = (*text_index).checked_add(1).ok_or_else(|| {
        AssetError::InvalidFormat("script text record count overflows u32".to_owned())
    })?;
    Ok(())
}

fn append_non_dialogue_nodes(
    nodes: &mut Vec<ScriptSourceNode>,
    labels: Vec<String>,
    words: &[u16],
    text_index: &mut u32,
    charset_map: &charset::CharsetMap,
) -> Result<(), AssetError> {
    let secondary_target = labels.iter().any(|label| label.starts_with("secondary_"));
    if secondary_target {
        if let Some(terminator) = glyph_string_end(words, 0, charset_map.glyph_count()) {
            return push_text_node(
                nodes,
                labels,
                text_index,
                ScriptTextGrammar::SecondaryString,
                Vec::new(),
                words[..terminator].to_vec(),
                words[terminator + 1..].to_vec(),
                charset_map,
            );
        }
    }

    let mut cursor = 0usize;
    let mut pending_labels = Some(labels);
    while cursor < words.len() {
        let found = (cursor..words.len()).find_map(|opcode_index| {
            inline_text_at(words, opcode_index, charset_map.glyph_count())
                .map(|(grammar, text_start, terminator)| (opcode_index, grammar, text_start, terminator))
        });
        let Some((opcode_index, grammar, text_start, terminator)) = found else {
            if cursor < words.len() {
                nodes.push(ScriptSourceNode::Raw {
                    labels: pending_labels.take().unwrap_or_default(),
                    words: words[cursor..].to_vec(),
                });
            }
            break;
        };
        if opcode_index > cursor {
            nodes.push(ScriptSourceNode::Raw {
                labels: pending_labels.take().unwrap_or_default(),
                words: words[cursor..opcode_index].to_vec(),
            });
        }
        push_text_node(
            nodes,
            pending_labels.take().unwrap_or_default(),
            text_index,
            grammar,
            vec![words[opcode_index]],
            words[text_start..terminator].to_vec(),
            Vec::new(),
            charset_map,
        )?;
        cursor = terminator + 1;
    }
    Ok(())
}

fn validate_source_text_coverage(
    source: &ScriptSourceDocument,
    charset_map: &charset::CharsetMap,
) -> Result<(), AssetError> {
    let mut expected_text_index = 0u32;
    for (node_index, node) in source.nodes.iter().enumerate() {
        match node {
            ScriptSourceNode::Raw { labels, words } => {
                if labels.iter().any(|label| label.starts_with("secondary_"))
                    && glyph_string_end(words, 0, charset_map.glyph_count()).is_some()
                {
                    return Err(AssetError::InvalidFormat(format!(
                        "script node {node_index} leaves a secondary-target string in raw IR; refusing extraction to avoid hidden text loss"
                    )));
                }
                if (0..words.len()).any(|index| inline_text_at(words, index, charset_map.glyph_count()).is_some()) {
                    return Err(AssetError::InvalidFormat(format!(
                        "script node {node_index} leaves a proven FF42/FF8C inline string in raw IR; refusing extraction to avoid hidden text loss"
                    )));
                }
            }
            ScriptSourceNode::Text {
                text_index,
                grammar,
                prefix_words,
                text,
                source_glyphs,
                suffix_words,
                labels,
            } => {
                if *text_index != expected_text_index {
                    return Err(AssetError::InvalidFormat(format!(
                        "script text record {text_index} appears where {expected_text_index} is required"
                    )));
                }
                if !source_glyphs.is_empty()
                    && charset_map.decode_slice(source_glyphs)?.as_str() != text.as_str()
                {
                    return Err(AssetError::InvalidFormat(format!(
                        "script text record {text_index} does not decode from its source glyph IDs"
                    )));
                }
                // A secondary-string target owns only the leading glyph run and its
                // FFFF terminator. The remaining words are an opaque machine tail
                // whose low-valued operands may fall inside the glyph-ID range and
                // may themselves be followed by FFFF. Treating that numerical shape
                // as text is unsound: entry 17 contains the proven tail
                // 0004 0019 000e ffff after text record 6. Dialogue FFFE coverage,
                // secondary-target anchoring, known inline opcodes and byte-exact
                // source round-trip provide the grammar safety checks instead.
                if suffix_words.contains(&0xfffe) {
                    return Err(AssetError::InvalidFormat(format!(
                        "script text record {text_index} hides an FFFE dialogue-page terminator in machine suffix words"
                    )));
                }
                if (0..suffix_words.len()).any(|index| inline_text_at(suffix_words, index, charset_map.glyph_count()).is_some()) {
                    return Err(AssetError::InvalidFormat(format!(
                        "script text record {text_index} leaves a proven FF42/FF8C inline string in machine suffix words"
                    )));
                }
                let grammar_valid = match grammar {
                    ScriptTextGrammar::SecondaryString => {
                        prefix_words.is_empty()
                            && labels.iter().any(|label| label.starts_with("secondary_"))
                    }
                    ScriptTextGrammar::InlineFf42 => {
                        prefix_words.as_slice() == [0xff42] && suffix_words.is_empty()
                    }
                    ScriptTextGrammar::InlineFf8c => {
                        prefix_words.as_slice() == [0xff8c] && suffix_words.is_empty()
                    }
                };
                if !grammar_valid {
                    return Err(AssetError::InvalidFormat(format!(
                        "script text record {text_index} has inconsistent grammar metadata"
                    )));
                }
                expected_text_index = expected_text_index.checked_add(1).ok_or_else(|| {
                    AssetError::InvalidFormat("script text record count overflows".to_owned())
                })?;
            }
            ScriptSourceNode::Dialogue { .. } => {}
        }
    }
    Ok(())
}

fn build_source_document(
    entry_id: u32,
    words: &[u16],
    stream_table: &ScriptOffsetTableAnnotation,
    primary_offsets: &[u32],
    secondary_records: &[ScriptSecondaryRecord],
    charset_map: &charset::CharsetMap,
) -> Result<ScriptSourceDocument, AssetError> {
    let table_bytes = usize::try_from(stream_table.table_bytes)
        .map_err(|_| AssetError::InvalidFormat("script stream table exceeds usize".to_owned()))?;
    let table_words = table_bytes / 2;
    if table_words > words.len() {
        return Err(AssetError::InvalidFormat(
            "script stream table exceeds payload words".to_owned(),
        ));
    }

    let text_ranges = parse_text_ranges(words, primary_offsets, charset_map.glyph_count())?;
    validate_text_coverage(words, &text_ranges, table_words)?;
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
    let mut next_text_index = 0u32;
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
                speaker: charset_map.decode_slice(&words[range.speaker_start..range.speaker_end])?,
                speaker_source_glyphs: words[range.speaker_start..range.speaker_end].to_vec(),
                pages: range
                    .pages
                    .iter()
                    .map(|page| {
                        let source_glyphs = words[page.start..page.end].to_vec();
                        Ok(ScriptDialoguePage {
                            text: charset_map.decode_slice(&source_glyphs)?,
                            source_glyphs,
                        })
                    })
                    .collect::<Result<Vec<_>, AssetError>>()?,
                screen_break_after_pages: range.screen_break_after_pages.clone(),
                screen_speaker_refresh_after_pages: range.screen_speaker_refresh_after_pages.clone(),
            });
        } else {
            append_non_dialogue_nodes(
                &mut nodes,
                node_labels,
                &words[start..end],
                &mut next_text_index,
                charset_map,
            )?;
        }
    }
    if !labels.is_empty() {
        return Err(AssetError::InvalidFormat(
            "script has relocation labels at the end of the payload".to_owned(),
        ));
    }

    let source = ScriptSourceDocument {
        document_version: SOURCE_DOCUMENT_VERSION,
        entry_id,
        // Extraction must stamp the recovered IR with the same charset used to
        // classify glyph words and decode dialogue text.  A previous patch left
        // this as the stock charset ID, so modified/expanded SC extraction
        // decoded successfully and then failed its own byte-exact round-trip
        // check in assemble_source_document().
        charset: charset_map.id().to_owned(),
        stream_count: u32::try_from(stream_table.entries.len()).unwrap_or(u32::MAX),
        nodes,
        secondary_records: source_secondary,
        relocation_model: "all located VM branch targets are stream IDs; stream offsets and primary marker offsets are directly proven, while corpus-classified secondary payload addresses are regenerated from labels"
            .to_owned(),
    };
    validate_source_text_coverage(&source, charset_map)?;
    Ok(source)
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
            "internal script IR version, entry ID, or charset does not match script state".to_owned(),
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
            ScriptSourceNode::Text {
                grammar,
                prefix_words,
                text,
                source_glyphs,
                suffix_words,
                ..
            } => {
                let encoded = encode_span_preserving_source(
                    text,
                    source_glyphs,
                    charset_map,
                )?;
                let maximum = match grammar {
                    ScriptTextGrammar::InlineFf42 => Some(0x15usize),
                    ScriptTextGrammar::InlineFf8c => Some(7usize),
                    ScriptTextGrammar::SecondaryString => None,
                };
                if let Some(maximum) = maximum {
                    if encoded.len() > maximum {
                        return Err(AssetError::InvalidProject(format!(
                            "script text record exceeds the {:?} engine display capacity of {maximum} glyphs",
                            grammar
                        )));
                    }
                }
                words.extend_from_slice(prefix_words);
                words.extend(encoded);
                words.push(0xffff);
                words.extend_from_slice(suffix_words);
            }
            ScriptSourceNode::Dialogue {
                marker_index,
                speaker,
                speaker_source_glyphs,
                pages,
                screen_break_after_pages,
                screen_speaker_refresh_after_pages,
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
                        "dialogue marker {marker_index} has no text"
                    )));
                }
                let speaker_words = encode_span_preserving_source(
                    speaker,
                    speaker_source_glyphs,
                    charset_map,
                )?;
                words.extend_from_slice(&speaker_words);
                words.push(0xffff);
                let screen_breaks = screen_break_after_pages
                    .iter()
                    .copied()
                    .collect::<BTreeSet<_>>();
                let speaker_refreshes = screen_speaker_refresh_after_pages
                    .iter()
                    .copied()
                    .collect::<BTreeSet<_>>();
                for (page_index, page) in pages.iter().enumerate() {
                    words.extend(encode_span_preserving_source(
                        &page.text,
                        &page.source_glyphs,
                        charset_map,
                    )?);
                    words.push(0xfffe);
                    let completed_pages = u32::try_from(page_index + 1).map_err(|_| {
                        AssetError::InvalidProject("dialogue page count exceeds u32".to_owned())
                    })?;
                    if screen_breaks.contains(&completed_pages) {
                        words.push(0xfffb);
                        // Runtime validation showed that FFFB advances to a new
                        // dialogue screen, but it does not automatically carry the
                        // nameplate/speaker buffer forward.  Generated VWF
                        // continuations therefore opt into repeating speaker +
                        // FFFF on screen 2+.  The explicit set preserves stock
                        // no-refresh scripts byte-for-byte.
                        if speaker_refreshes.contains(&completed_pages) {
                            words.extend_from_slice(&speaker_words);
                            words.push(0xffff);
                        }
                    }
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
        "The internal script IR is a lossless relocatable source-equivalent representation: scenario-dialogue.json supplies user text, while control/data not yet assigned proven semantics remains in machine-managed raw u16 nodes and engine-consumed payload addresses remain labels"
            .to_owned(),
        "Speaker and dialogue pages may change encoded length; build preserves page boundaries and regenerates the stream table, primary marker table, and payload-address fields in secondary records"
            .to_owned(),
        "The seven secondary-record fields are not given speculative semantic names; corpus-wide separation identifies payload addresses versus small immediate values without claiming their higher-level purpose"
            .to_owned(),
        "The zero-filled gap before the 16-byte SC integrity footer is reusable payload capacity; FUN_8102B4AC verifies the footer as two seeded 64-bit additive checksums, and build regenerates it after every edit"
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
        input.resize(allocation - SC_INTEGRITY_FOOTER_SIZE, 0);
        let footer = compute_sc_integrity_footer(&input).unwrap();
        input.extend_from_slice(&footer);
        input
    }

    #[test]
    fn sc_integrity_footer_matches_engine_lane_sums() {
        let mut input = Vec::new();
        for word in [1u32, 2, 3, 4] {
            input.extend_from_slice(&word.to_le_bytes());
        }
        assert_eq!(
            compute_sc_integrity_footer(&input).unwrap(),
            [
                0x12, 0x11, 0x11, 0x11, 0x13, 0x11, 0x11, 0x11,
                0x14, 0x11, 0x11, 0x11, 0x15, 0x11, 0x11, 0x11,
            ]
        );
    }

    #[test]
    fn sc_integrity_footer_changes_after_same_length_edit() {
        let mut input = vec![0u8; 0x20];
        let original = compute_sc_integrity_footer(&input).unwrap();
        input[0] = 1;
        let edited = compute_sc_integrity_footer(&input).unwrap();
        assert_ne!(original, edited);
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
        let mut source = build_source_document(7, &words, &table, &offsets, &[], charset::default_map()).unwrap();
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
        let source = build_source_document(86, &words, &table, &offsets, &[], charset::default_map()).unwrap();
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
    fn residual_fffe_outside_dialogue_is_rejected() {
        let words = vec![
            4, 0,
            0xfff0, 0, 0xffff,
            0x016e, 0xfffe,
            0xfef4, 0xfffe, 0xffff,
        ];
        let table = parse_offset_table(&words_to_bytes(&words)).unwrap();
        let offsets = derive_primary_offsets(&words).unwrap();
        let error = build_source_document(86, &words, &table, &offsets, &[], charset::default_map())
            .expect_err("residual FFFE must fail closed");
        assert!(error.to_string().contains("hidden text loss"));
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
        let source = build_source_document(86, &words, &table, &offsets, &[], charset::default_map()).unwrap();
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
        let source = build_source_document(5, &words, &table, &offsets, &[], charset::default_map()).unwrap();
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
    fn unanchored_low_operands_remain_raw_and_round_trip() {
        let source = ScriptSourceDocument {
            document_version: SOURCE_DOCUMENT_VERSION,
            entry_id: 9,
            charset: charset::default_map().id().to_owned(),
            stream_count: 1,
            nodes: vec![ScriptSourceNode::Raw {
                labels: vec!["stream_0000".to_owned()],
                words: vec![0x0004, 0x0019, 0x000e, 0xffff],
            }],
            secondary_records: Vec::new(),
            relocation_model: String::new(),
        };
        validate_source_text_coverage(&source, charset::default_map()).unwrap();
        let (words, secondary) =
            assemble_source_document(9, &source, charset::default_map()).unwrap();
        assert!(secondary.is_empty());
        assert_eq!(words, vec![4, 0, 0x0004, 0x0019, 0x000e, 0xffff]);
    }

    #[test]
    fn edited_non_dialogue_text_reencodes_without_source_glyphs() {
        let source = ScriptSourceDocument {
            document_version: SOURCE_DOCUMENT_VERSION,
            entry_id: 9,
            charset: charset::default_map().id().to_owned(),
            stream_count: 1,
            nodes: vec![ScriptSourceNode::Text {
                labels: vec![
                    "stream_0000".to_owned(),
                    "secondary_000_0_target".to_owned(),
                ],
                text_index: 0,
                grammar: ScriptTextGrammar::SecondaryString,
                prefix_words: Vec::new(),
                text: "レム".to_owned(),
                source_glyphs: Vec::new(),
                suffix_words: vec![0, 4, 0xffff],
            }],
            secondary_records: Vec::new(),
            relocation_model: String::new(),
        };
        validate_source_text_coverage(&source, charset::default_map()).unwrap();
        let (words, secondary) =
            assemble_source_document(9, &source, charset::default_map()).unwrap();
        assert!(secondary.is_empty());
        assert_eq!(words, vec![4, 0, 0x016e, 0x0162, 0xffff, 0, 4, 0xffff]);
    }

    #[test]
    fn secondary_machine_suffix_low_operands_are_not_reclassified_as_text() {
        let source = ScriptSourceDocument {
            document_version: SOURCE_DOCUMENT_VERSION,
            entry_id: 17,
            charset: charset::default_map().id().to_owned(),
            stream_count: 1,
            nodes: vec![ScriptSourceNode::Text {
                labels: vec![
                    "stream_0000".to_owned(),
                    "secondary_002_5_target".to_owned(),
                ],
                text_index: 0,
                grammar: ScriptTextGrammar::SecondaryString,
                prefix_words: Vec::new(),
                text: "キスといえば口".to_owned(),
                source_glyphs: vec![0x012f, 0x013b, 0x00f7, 0x00d3, 0x00d7, 0x00ff, 0x0537],
                suffix_words: vec![0x0004, 0x0019, 0x000e, 0xffff],
            }],
            secondary_records: Vec::new(),
            relocation_model: String::new(),
        };
        validate_source_text_coverage(&source, charset::default_map()).unwrap();
        let (words, secondary) =
            assemble_source_document(17, &source, charset::default_map()).unwrap();
        assert!(secondary.is_empty());
        assert_eq!(
            words,
            vec![
                4, 0,
                0x012f, 0x013b, 0x00f7, 0x00d3, 0x00d7, 0x00ff, 0x0537, 0xffff,
                0x0004, 0x0019, 0x000e, 0xffff,
            ]
        );
    }

    #[test]
    fn secondary_target_string_is_exposed_and_round_trips() {
        let mut nodes = Vec::new();
        let mut text_index = 0;
        append_non_dialogue_nodes(
            &mut nodes,
            vec!["stream_0000".to_owned(), "secondary_000_0_target".to_owned()],
            &[0x013b, 0x0152, 0xffff, 0, 4],
            &mut text_index,
        )
        .unwrap();
        assert_eq!(text_index, 1);
        assert!(matches!(
            &nodes[0],
            ScriptSourceNode::Text {
                text_index: 0,
                grammar: ScriptTextGrammar::SecondaryString,
                text,
                suffix_words,
                ..
            } if text == "スバ" && suffix_words.as_slice() == [0, 4]
        ));
        let source = ScriptSourceDocument {
            document_version: SOURCE_DOCUMENT_VERSION,
            entry_id: 9,
            charset: charset::default_map().id().to_owned(),
            stream_count: 1,
            nodes,
            secondary_records: Vec::new(),
            relocation_model: String::new(),
        };
        let (words, secondary) =
            assemble_source_document(9, &source, charset::default_map()).unwrap();
        assert!(secondary.is_empty());
        assert_eq!(words, vec![4, 0, 0x013b, 0x0152, 0xffff, 0, 4]);
    }

    #[test]
    fn ff42_and_ff8c_inline_strings_are_exposed_and_round_trip() {
        let raw = vec![
            0xfffc, 0xff42, 0x013b, 0x0152, 0xffff, 0xff41,
            0xff8c, 0x016e, 0x0162, 0xffff,
        ];
        let mut nodes = Vec::new();
        let mut text_index = 0;
        append_non_dialogue_nodes(
            &mut nodes,
            vec!["stream_0000".to_owned()],
            &raw,
            &mut text_index,
        )
        .unwrap();
        assert_eq!(text_index, 2);
        assert!(nodes.iter().any(|node| matches!(
            node,
            ScriptSourceNode::Text {
                grammar: ScriptTextGrammar::InlineFf42,
                text,
                ..
            } if text == "スバ"
        )));
        assert!(nodes.iter().any(|node| matches!(
            node,
            ScriptSourceNode::Text {
                grammar: ScriptTextGrammar::InlineFf8c,
                text,
                ..
            } if text == "レム"
        )));
        let source = ScriptSourceDocument {
            document_version: SOURCE_DOCUMENT_VERSION,
            entry_id: 87,
            charset: charset::default_map().id().to_owned(),
            stream_count: 1,
            nodes,
            secondary_records: Vec::new(),
            relocation_model: String::new(),
        };
        let (words, secondary) =
            assemble_source_document(87, &source, charset::default_map()).unwrap();
        assert!(secondary.is_empty());
        let mut expected = vec![4, 0];
        expected.extend(raw);
        assert_eq!(words, expected);
    }

    #[test]
    fn compact_state_contains_no_duplicate_text_and_hydrates_exactly() {
        let source = ScriptSourceDocument {
            document_version: SOURCE_DOCUMENT_VERSION,
            entry_id: 86,
            charset: charset::default_map().id().to_owned(),
            stream_count: 1,
            nodes: vec![ScriptSourceNode::Dialogue {
                labels: vec!["stream_0000".to_owned()],
                marker_index: 0,
                speaker: "ー".to_owned(),
                speaker_source_glyphs: vec![0x001b],
                pages: vec![ScriptDialoguePage {
                    text: "ー".to_owned(),
                    source_glyphs: vec![0x001b],
                }],
                screen_break_after_pages: Vec::new(),
                screen_speaker_refresh_after_pages: Vec::new(),
            }],
            secondary_records: Vec::new(),
            relocation_model: "test".to_owned(),
        };
        let machine = compact_source_document(&source, charset::default_map()).unwrap();
        let encoded = serde_json::to_value(&machine).unwrap();
        let nodes = encoded["nodes"].as_array().unwrap();
        for node in nodes {
            let object = node.as_object().unwrap();
            for forbidden in [
                "text",
                "speaker",
                "speaker_text",
                "pages",
                "page_texts",
                "source_glyphs",
                "speaker_source_glyphs",
            ] {
                assert!(!object.contains_key(forbidden));
            }
        }
        let dialogue = ScenarioDialogueEntry {
            entry_id: 86,
            dialogues: vec![ScenarioDialogueItem {
                marker_index: 0,
                speaker: "ー".to_owned(),
                text: "ー".to_owned(),
                engine_lines_override: None,
                engine_screens_override: None,
                row_joiners_override: None,
                generated_marker_count_override: None,
            }],
            texts: Vec::new(),
        };
        let hydrated = hydrate_machine_source(
            &machine,
            &dialogue,
            &[],
            charset::default_map(),
        )
        .unwrap();
        let original = assemble_source_document(86, &source, charset::default_map()).unwrap();
        let rebuilt = assemble_source_document(86, &hydrated, charset::default_map()).unwrap();
        assert_eq!(rebuilt, original);
        let ScriptMachineNode::Dialogue {
            speaker_glyph_aliases,
            page_glyph_aliases,
            ..
        } = &machine.nodes[0]
        else {
            panic!("expected dialogue machine node");
        };
        assert_eq!(speaker_glyph_aliases.len(), 1);
        assert_eq!(page_glyph_aliases[0].len(), 1);
    }

    #[test]
    fn portable_metadata_folds_generated_markers_without_copying_text() {
        fn dialogue(marker_index: u32) -> ScriptSourceNode {
            ScriptSourceNode::Dialogue {
                labels: Vec::new(),
                marker_index,
                speaker: "ー".to_owned(),
                speaker_source_glyphs: vec![0x001b],
                pages: vec![ScriptDialoguePage {
                    text: "ー".to_owned(),
                    source_glyphs: vec![0x001b],
                }],
                screen_break_after_pages: Vec::new(),
                screen_speaker_refresh_after_pages: Vec::new(),
            }
        }
        fn invocation(marker_index: u16) -> ScriptSourceNode {
            ScriptSourceNode::Raw {
                labels: Vec::new(),
                words: vec![0xfffb, 0xff68, marker_index, 2],
            }
        }
        let source = ScriptSourceDocument {
            document_version: SOURCE_DOCUMENT_VERSION,
            entry_id: 86,
            charset: charset::default_map().id().to_owned(),
            stream_count: 1,
            nodes: vec![
                dialogue(0),
                invocation(0),
                dialogue(1),
                invocation(1),
                dialogue(2),
                invocation(2),
                dialogue(3),
            ],
            secondary_records: Vec::new(),
            relocation_model: "test".to_owned(),
        };
        let metadata = [(
            (86, 0),
            ScenarioDialogueMetadataItem {
                marker_index: 0,
                row_joiners: vec![" ".to_owned(), " ".to_owned()],
                generated_marker_count: 2,
            },
        )]
        .into_iter()
        .collect::<BTreeMap<_, _>>();
        let entry = build_dialogue_entry_from_source(&source, &metadata).unwrap();
        assert_eq!(entry.dialogues.len(), 2);
        assert_eq!(entry.dialogues[0].marker_index, 0);
        assert_eq!(entry.dialogues[0].text, "ー ー ー");
        assert_eq!(entry.dialogues[0].source_generated_marker_count(), 2);
        assert_eq!(entry.dialogues[1].marker_index, 1);
        assert_eq!(entry.dialogues[1].text, "ー");
    }

    #[test]
    fn folded_metadata_rehydrates_existing_generated_marker_chain() {
        fn dialogue(marker_index: u32) -> ScriptMachineNode {
            ScriptMachineNode::Dialogue {
                labels: Vec::new(),
                marker_index,
                speaker_glyph_aliases: Vec::new(),
                page_char_counts: vec![1],
                page_glyph_aliases: vec![Vec::new()],
                screen_break_after_pages: Vec::new(),
                screen_speaker_refresh_after_pages: Vec::new(),
            }
        }
        fn invocation(marker_index: u16) -> ScriptMachineNode {
            ScriptMachineNode::Raw {
                labels: Vec::new(),
                words: vec![0xfffb, 0xff68, marker_index, 2],
            }
        }
        let source = ScriptMachineDocument {
            document_version: SOURCE_DOCUMENT_VERSION,
            entry_id: 86,
            stream_count: 1,
            nodes: vec![
                dialogue(0),
                invocation(0),
                dialogue(1),
                invocation(1),
                dialogue(2),
                invocation(2),
                dialogue(3),
            ],
            relocation_model: "test".to_owned(),
        };
        let edited = ScenarioDialogueEntry {
            entry_id: 86,
            dialogues: vec![
                ScenarioDialogueItem {
                    marker_index: 0,
                    speaker: "ー".to_owned(),
                    text: "ー ー ー".to_owned(),
                    engine_lines_override: None,
                    engine_screens_override: None,
                    row_joiners_override: Some(vec![" ".to_owned(), " ".to_owned()]),
                    generated_marker_count_override: Some(2),
                },
                ScenarioDialogueItem {
                    marker_index: 1,
                    speaker: "ー".to_owned(),
                    text: "ー".to_owned(),
                    engine_lines_override: None,
                    engine_screens_override: None,
                    row_joiners_override: Some(Vec::new()),
                    generated_marker_count_override: Some(0),
                },
            ],
            texts: Vec::new(),
        };
        let hydrated = hydrate_machine_source(
            &source,
            &edited,
            &[],
            charset::default_map(),
        )
        .unwrap();
        let markers = hydrated
            .nodes
            .iter()
            .filter_map(|node| match node {
                ScriptSourceNode::Dialogue { marker_index, .. } => Some(*marker_index),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(markers, vec![0, 1, 2, 3]);
        let targets = hydrated
            .nodes
            .iter()
            .filter_map(simple_dialogue_invocation_target)
            .collect::<Vec<_>>();
        assert_eq!(targets, vec![0, 1, 2]);
    }

    #[test]
    fn scenario_navigation_is_startup_first_and_dependency_stable() {
        fn source(entry_id: u32) -> ScriptSourceDocument {
            ScriptSourceDocument {
                document_version: SOURCE_DOCUMENT_VERSION,
                entry_id,
                charset: charset::default_map().id().to_owned(),
                stream_count: 1,
                nodes: Vec::new(),
                secondary_records: Vec::new(),
                relocation_model: String::new(),
            }
        }

        let documents = [0u32, 1, 2, 86, 87, 88]
            .into_iter()
            .map(|entry_id| {
                (
                    entry_id,
                    (
                        entry_id,
                        format!("{entry_id:05}.script.json"),
                        source(entry_id),
                    ),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let transitions = vec![
            ScenarioTransition {
                source_entry_id: 2,
                source_node_index: 0,
                source_word_index: 0,
                target_entry_id: 0,
                target_stream_id: 0,
                opcode: "FFEF".to_owned(),
            },
            ScenarioTransition {
                source_entry_id: 86,
                source_node_index: 0,
                source_word_index: 0,
                target_entry_id: 2,
                target_stream_id: 0,
                opcode: "FFEF".to_owned(),
            },
            ScenarioTransition {
                source_entry_id: 88,
                source_node_index: 0,
                source_word_index: 0,
                target_entry_id: 87,
                target_stream_id: 0,
                opcode: "FFEF".to_owned(),
            },
        ];
        let navigation = build_scenario_navigation(&documents, &transitions, 86);
        assert_eq!(
            navigation.iter().map(|item| item.entry_id).collect::<Vec<_>>(),
            vec![86, 2, 0, 1, 88, 87]
        );
        assert_eq!(
            navigation
                .iter()
                .map(|item| item.navigation_order)
                .collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4, 5]
        );
        assert!(navigation[..3].iter().all(|item| item.reachable_from_startup));
        assert!(navigation[3..].iter().all(|item| !item.reachable_from_startup));
    }

    #[test]
    fn scenario_navigation_delays_shared_convergence_until_all_predecessors() {
        fn source(entry_id: u32) -> ScriptSourceDocument {
            ScriptSourceDocument {
                document_version: SOURCE_DOCUMENT_VERSION,
                entry_id,
                charset: charset::default_map().id().to_owned(),
                stream_count: 1,
                nodes: Vec::new(),
                secondary_records: Vec::new(),
                relocation_model: String::new(),
            }
        }

        let documents = [54u32, 56, 59, 64, 67, 73]
            .into_iter()
            .map(|entry_id| {
                (
                    entry_id,
                    (
                        entry_id,
                        format!("{entry_id:05}.script.json"),
                        source(entry_id),
                    ),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let transition = |source_entry_id, target_entry_id| ScenarioTransition {
            source_entry_id,
            source_node_index: 0,
            source_word_index: 0,
            target_entry_id,
            target_stream_id: 0,
            opcode: "FFEF".to_owned(),
        };
        let transitions = vec![
            transition(54, 56),
            transition(54, 59),
            transition(56, 64),
            transition(59, 67),
            transition(64, 73),
            transition(67, 73),
        ];
        let navigation = build_scenario_navigation(&documents, &transitions, 54);
        assert_eq!(
            navigation.iter().map(|item| item.entry_id).collect::<Vec<_>>(),
            vec![54, 56, 59, 64, 67, 73]
        );
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
