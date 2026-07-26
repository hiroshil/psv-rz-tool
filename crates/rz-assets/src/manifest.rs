use serde::{Deserialize, Serialize};

pub const PROJECT_SCHEMA_VERSION: u32 = 1;
pub const PROJECT_FILE_NAME: &str = "rz-project.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ProjectMode {
    Editable,
    RawOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectManifest {
    pub schema_version: u32,
    pub mode: ProjectMode,
    pub source_name: String,
    pub source: ProjectSource,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "source_type", rename_all = "kebab-case")]
pub enum ProjectSource {
    Cpk {
        cpk: CpkProject,
        assets: Vec<AssetEntry>,
    },
    LtFont {
        document: String,
    },
    RawFile {
        path: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CpkProject {
    pub alignment: u16,
    pub index_mode: IndexMode,
    pub direct_itoc: bool,
    pub version: u16,
    pub revision: u16,
    pub update_date_time: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IndexMode {
    Toc,
    Itoc,
    TocAndItoc,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetEntry {
    pub order: u32,
    pub directory: String,
    pub file_name: String,
    pub id: Option<u32>,
    pub user_string: String,
    #[serde(flatten)]
    pub kind: AssetKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "asset_type", rename_all = "kebab-case")]
pub enum AssetKind {
    Raw {
        path: String,
    },
    Opaque {
        path: String,
        reason: String,
    },
    EngineImagePackage {
        document: String,
    },
    Script {
        document: String,
    },
    Gxt {
        images: Vec<String>,
        metadata: GxtMetadata,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GxtMetadata {
    pub version: u32,
    pub textures: Vec<GxtTextureMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GxtTextureMetadata {
    pub palette_index: u32,
    pub flags: u32,
    pub texture_type: u32,
    pub format: u32,
    pub width: u16,
    pub height: u16,
    pub storage_width: u16,
    pub storage_height: u16,
    pub mip_count: u8,
}
