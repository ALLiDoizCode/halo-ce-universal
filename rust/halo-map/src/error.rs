use std::fmt;

/// Why a map file could not be loaded.
#[derive(Debug)]
pub enum MapError {
    /// The file could not be read.
    Io(std::io::Error),
    /// The bytes are not a Halo cache file, or are cut short or inconsistent.
    Malformed(String),
    /// The cache file version is not the one the Xbox maps use (5).
    UnsupportedVersion(i32),
    /// The zlib stream would not inflate to the size the header states.
    Decompress(String),
    /// A collision block holds indices outside the block they refer to.
    IndexOutOfRange { field: &'static str, count: usize },
}

impl fmt::Display for MapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MapError::Io(e) => write!(f, "cannot read map file: {e}"),
            MapError::Malformed(what) => write!(f, "malformed map file: {what}"),
            MapError::UnsupportedVersion(v) => {
                write!(f, "cache file version {v} (the Xbox maps are version 5)")
            }
            MapError::Decompress(what) => write!(f, "cannot inflate map file: {what}"),
            MapError::IndexOutOfRange { field, count } => {
                write!(f, "collision data: {count} out-of-range indices in {field}")
            }
        }
    }
}

impl std::error::Error for MapError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            MapError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for MapError {
    fn from(e: std::io::Error) -> Self {
        MapError::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, MapError>;

pub(crate) fn malformed<T>(what: impl Into<String>) -> Result<T> {
    Err(MapError::Malformed(what.into()))
}
