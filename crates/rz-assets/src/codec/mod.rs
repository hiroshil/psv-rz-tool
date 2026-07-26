pub mod charset;
pub mod engine_package;
pub mod gxt;
pub mod lt_font;
pub mod script;

use std::fs;
use std::path::Path;

use crate::engine_allocations;
use crate::error::AssetError;
use crate::manifest::AssetKind;
use engine_package::EnginePackageProfile;

const TARGET_ARCHIVES: &[&str] = &[
    "sc.cpk",
    "addpt.cpk",
    "bk.cpk",
    "bsf.cpk",
    "pt.cpk",
];

pub struct DecodeContext<'a> {
    pub archive_name: &'a str,
    pub directory: &'a str,
    pub file_name: &'a str,
    pub order: u32,
    pub id: Option<u32>,
}

pub fn decode_editable(
    input: &[u8],
    context: &DecodeContext<'_>,
    asset_directory: &Path,
    output_stem: &str,
) -> Result<AssetKind, AssetError> {
    ensure_in_scope(context, input)?;
    let archive = context.archive_name.to_ascii_lowercase();
    let entry_id = context.id.unwrap_or(context.order);
    let allocation_size = engine_allocations::allocation_size(&archive, entry_id).ok_or_else(|| {
        AssetError::UnsupportedAsset {
            entry: display_entry(context),
            reason: format!(
                "entry ID {entry_id} is absent from the executable-resident sector table"
            ),
        }
    })?;

    if archive == "sc.cpk" {
        return script::decode(
            input,
            asset_directory,
            output_stem,
            entry_id,
            allocation_size,
        );
    }

    let profile = match archive.as_str() {
        "addpt.cpk" => Some(EnginePackageProfile::AddPt),
        "bk.cpk" => Some(EnginePackageProfile::Bk),
        "bsf.cpk" => Some(EnginePackageProfile::Bsf),
        "pt.cpk" => Some(EnginePackageProfile::Pt),
        _ => None,
    };
    if let Some(profile) = profile {
        return decode_engine_package_or_opaque(
            input,
            asset_directory,
            output_stem,
            profile,
            entry_id,
            allocation_size,
        );
    }

    Err(AssetError::UnsupportedAsset {
        entry: display_entry(context),
        reason: "entry did not match the engine grammar selected by its archive".to_owned(),
    })
}

fn decode_engine_package_or_opaque(
    input: &[u8],
    asset_directory: &Path,
    output_stem: &str,
    profile: EnginePackageProfile,
    entry_id: u32,
    allocation_size: usize,
) -> Result<AssetKind, AssetError> {
    // Decode transactionally at entry granularity. A CPK can contain engine-
    // ignored placeholders or records whose grammar is not established by any
    // call-site. Such entries remain explicit opaque assets instead of making
    // the complete archive unusable or silently guessing a format.
    let staging = asset_directory.join(format!(".{output_stem}.rz-entry-decode"));
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging)?;
    match engine_package::decode(
        input,
        &staging,
        output_stem,
        profile,
        entry_id,
        allocation_size,
    ) {
        Ok(kind) => {
            promote_directory(&staging, asset_directory)?;
            fs::remove_dir(&staging)?;
            Ok(kind)
        }
        Err(AssetError::InvalidFormat(reason)) => {
            fs::remove_dir_all(&staging)?;
            let path = format!("{output_stem}.opaque.bin");
            fs::write(asset_directory.join(&path), input)?;
            Ok(AssetKind::Opaque {
                path,
                reason: format!(
                    "no editable grammar was proven for this entry; parser evidence rejected it: {reason}"
                ),
            })
        }
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            Err(error)
        }
    }
}

fn promote_directory(source: &Path, destination: &Path) -> Result<(), AssetError> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if target.exists() {
            return Err(AssetError::InvalidProject(format!(
                "decode staging target already exists: {}",
                target.display()
            )));
        }
        fs::rename(entry.path(), target)?;
    }
    Ok(())
}

pub fn encode(kind: &AssetKind, project_root: &Path) -> Result<Vec<u8>, AssetError> {
    match kind {
        AssetKind::Raw { path } | AssetKind::Opaque { path, .. } => {
            Ok(std::fs::read(project_root.join(path))?)
        }
        AssetKind::EngineImagePackage { document } => {
            engine_package::encode(&project_root.join(document))
        }
        AssetKind::Script { document } => script::encode(&project_root.join(document)),
        AssetKind::Gxt { images, metadata } => gxt::encode(project_root, images, metadata),
    }
}

pub(crate) fn ensure_in_scope(
    context: &DecodeContext<'_>,
    input: &[u8],
) -> Result<(), AssetError> {
    let lower = context.file_name.to_ascii_lowercase();
    let archive = context.archive_name.to_ascii_lowercase();
    if !TARGET_ARCHIVES.contains(&archive.as_str()) {
        return Err(AssetError::OutOfScopeResource(context.archive_name.to_owned()));
    }
    let audio_extension = [".acb", ".awb", ".adx", ".hca", ".aax"]
        .iter()
        .any(|extension| lower.ends_with(extension));
    let video_extension = [".usm", ".sfd"]
        .iter()
        .any(|extension| lower.ends_with(extension));
    let audio_magic = input.starts_with(b"AFS2")
        || input.starts_with(b"HCA\0")
        || input.starts_with(b"@AHX")
        || lower.ends_with(".adx");
    let video_magic = input.starts_with(b"CRID") || input.starts_with(b"@SFV");

    if audio_extension || audio_magic {
        return Err(AssetError::OutOfScopeAsset {
            entry: display_entry(context),
            kind: "an audio asset",
        });
    }
    if video_extension || video_magic {
        return Err(AssetError::OutOfScopeAsset {
            entry: display_entry(context),
            kind: "a video asset",
        });
    }
    Ok(())
}

fn display_entry(context: &DecodeContext<'_>) -> String {
    if context.directory.is_empty() {
        context.file_name.to_owned()
    } else {
        format!("{}/{}", context.directory, context.file_name)
    }
}
