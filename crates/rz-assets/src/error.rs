use std::error::Error;
use std::fmt::{Display, Formatter};
use std::path::PathBuf;

#[derive(Debug)]
pub enum AssetError {
    OutputExists(PathBuf),
    OutOfScopeResource(String),
    InvalidProject(String),
    UnsupportedAsset { entry: String, reason: String },
    OutOfScopeAsset { entry: String, kind: &'static str },
    InvalidFormat(String),
    Io(std::io::Error),
    Json(serde_json::Error),
    Image(image::ImageError),
    Archive(Box<dyn Error>),
}

impl Display for AssetError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutputExists(path) => write!(f, "output already exists: {}", path.display()),
            Self::OutOfScopeResource(resource) => write!(
                f,
                "resource {resource:?} is outside the engine texture/image/script set; use cri-cpk-cli only for generic CPK access"
            ),
            Self::InvalidProject(message) => write!(f, "invalid project: {message}"),
            Self::UnsupportedAsset { entry, reason } => {
                write!(f, "unsupported target asset {entry}: {reason}")
            }
            Self::OutOfScopeAsset { entry, kind } => {
                write!(f, "{entry} is {kind}; audio/video is intentionally outside this tool")
            }
            Self::InvalidFormat(message) => write!(f, "invalid asset format: {message}"),
            Self::Io(error) => Display::fmt(error, f),
            Self::Json(error) => Display::fmt(error, f),
            Self::Image(error) => Display::fmt(error, f),
            Self::Archive(error) => Display::fmt(error, f),
        }
    }
}

impl Error for AssetError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Image(error) => Some(error),
            Self::Archive(error) => Some(error.as_ref()),
            _ => None,
        }
    }
}

impl From<std::io::Error> for AssetError {
    fn from(value: std::io::Error) -> Self { Self::Io(value) }
}
impl From<serde_json::Error> for AssetError {
    fn from(value: serde_json::Error) -> Self { Self::Json(value) }
}
impl From<image::ImageError> for AssetError {
    fn from(value: image::ImageError) -> Self { Self::Image(value) }
}
