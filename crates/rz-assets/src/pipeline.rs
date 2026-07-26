use std::collections::BTreeSet;
use std::error::Error;
use std::fs::{self, File};
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};

use cri_archive_lib::cpk::encrypt::data::DummyDecryptor;
use cri_archive_lib::cpk::reader::{CpkMetadata, CpkReader};
use cri_archive_lib::cpk::writer::{
    CpkIndexMode, CpkInputFile, CpkWriter, CpkWriterOptions, CpkWriterProfile,
};

use crate::codec::{self, DecodeContext};
use crate::eboot::{self, EbootPatchReport, ScEntryPatch, ScPatchPlan, SC_ENTRY_COUNT};
use crate::engine_allocations;
use crate::error::AssetError;
use crate::manifest::{
    AssetEntry, AssetKind, CpkProject, IndexMode, ProjectManifest, ProjectMode,
    ProjectSource, PROJECT_FILE_NAME, PROJECT_SCHEMA_VERSION,
};

#[derive(Debug, Clone, Copy, Default)]
pub struct ExtractOptions {
    pub raw_only: bool,
    pub debug_script_ir: bool,
}

#[derive(Debug, Clone, Default)]
pub struct BuildOptions {
    pub eboot_in: Option<PathBuf>,
    pub eboot_out: Option<PathBuf>,
    pub charset_map: Option<PathBuf>,
}


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
}

struct BuildOutcome {
    report: BuildReport,
    sc_patch_plan: Option<ScPatchPlan>,
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
        let bytes = fs::read(input)?;
        let document = codec::lt_font::decode(&bytes, stage)?;
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
    if options.debug_script_ir && (!is_sc || options.raw_only) {
        return Err(AssetError::InvalidProject(
            "--debug-script-ir is available only for editable sc.cpk extraction".to_owned(),
        ));
    }
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
            codec::decode_editable(&extracted, &context, stage, &output_stem)?
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
            codec::charset::write_default_document(&stage.join("charset.json"))?;
            let presentation_order = codec::script::write_routing_document(
                stage,
                &assets,
                options.debug_script_ir,
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

    let manifest_path = project_directory.join(PROJECT_FILE_NAME);
    let manifest: ProjectManifest = serde_json::from_slice(&fs::read(&manifest_path)?)?;
    validate_manifest(&manifest)?;

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
                if options.eboot_in.is_some() || options.charset_map.is_some() {
                    return Err(AssetError::InvalidProject(
                        "--eboot-in/--eboot-out/--charset-map are valid only when building sc.cpk".to_owned(),
                    ));
                }
                let bytes = codec::lt_font::encode(&project_directory.join(document))?;
                fs::write(&staged_output, &bytes)?;
                Ok(BuildOutcome {
                    report: BuildReport {
                        files: 1,
                        output_size: u64::try_from(bytes.len()).unwrap(),
                        eboot_patch: None,
                    },
                    sc_patch_plan: None,
                })
            }
            ProjectSource::RawFile { path } => {
                if options.eboot_in.is_some() || options.charset_map.is_some() {
                    return Err(AssetError::InvalidProject(
                        "--eboot-in/--eboot-out/--charset-map are valid only when building sc.cpk".to_owned(),
                    ));
                }
                let bytes = fs::read(project_directory.join(path))?;
                fs::write(&staged_output, &bytes)?;
                Ok(BuildOutcome {
                    report: BuildReport {
                        files: 1,
                        output_size: u64::try_from(bytes.len()).unwrap(),
                        eboot_patch: None,
                    },
                    sc_patch_plan: None,
                })
            }
        }
    })();

    let _ = fs::remove_dir_all(&stage);
    let mut staged_eboot = None::<PathBuf>;
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
                    let patch_report = match eboot::patch_sc_elf(input, &stage_eboot, plan) {
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
            } else if options.eboot_in.is_some() || options.charset_map.is_some() {
                let _ = fs::remove_file(&staged_output);
                return Err(AssetError::InvalidProject(
                    "--eboot-in/--eboot-out/--charset-map are valid only for an editable sc.cpk project".to_owned(),
                ));
            }

            if let Err(error) = fs::rename(&staged_output, output) {
                let _ = fs::remove_file(&staged_output);
                if let Some(path) = staged_eboot.as_ref() {
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
                    return Err(AssetError::Io(error));
                }
            }
            Ok(outcome.report)
        }
        Err(error) => {
            let _ = fs::remove_file(&staged_output);
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
            Some(path) => codec::charset::load_document(&path)?,
            None => codec::charset::default_map().clone(),
        })
    } else {
        None
    };
    let scenario_dialogues = if is_sc {
        let entries = codec::script::load_dialogue_document(project_directory)?;
        if entries.len() != SC_ENTRY_COUNT {
            return Err(AssetError::InvalidProject(format!(
                "scenario-dialogue.json contains {} entries, expected {SC_ENTRY_COUNT}",
                entries.len()
            )));
        }
        Some(entries)
    } else {
        None
    };
    let scenario_states = if is_sc {
        let entries = codec::script::load_state_bundle(project_directory)?;
        if entries.len() != SC_ENTRY_COUNT {
            return Err(AssetError::InvalidProject(format!(
                ".rz-internal/sc-state.json.gz contains {} entries, expected {SC_ENTRY_COUNT}",
                entries.len()
            )));
        }
        Some(entries)
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
            if document != ".rz-internal/sc-state.json.gz" {
                return Err(AssetError::InvalidProject(format!(
                    "sc.cpk entry {} does not reference the compact SC state bundle",
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
                        ".rz-internal/sc-state.json.gz omits engine entry {entry_id}"
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
            let allocation = stock_allocation.max(info.required_allocation);
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
        },
        sc_patch_plan: is_sc.then_some(ScPatchPlan { entries: sc_entries }),
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
