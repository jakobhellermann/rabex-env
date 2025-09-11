pub mod binary_catalog;

use serde_derive::Deserialize;
use std::path::{Path, PathBuf};

#[allow(non_snake_case)]
#[derive(Debug, Deserialize)]
pub struct AddressablesSettings {
    pub m_buildTarget: String,
    pub m_SettingsHash: String,
    pub m_CatalogLocations: Vec<CatalogLocation>,
    pub m_LogResourceManagerExceptions: bool,
    pub m_ExtraInitializationData: Vec<()>,
    pub m_DisableCatalogUpdateOnStart: bool,
    pub m_IsLocalCatalogInBundle: bool,
    pub m_CertificateHandlerType: AssemblyClass,
    pub m_AddressablesVersion: String,
    pub m_maxConcurrentWebRequests: u32,
    pub m_CatalogRequestsTimeout: u32,
}
impl AddressablesSettings {
    /// Build folder relative to `game_Data` folder
    pub fn build_folder(&self) -> PathBuf {
        Path::new("StreamingAssets/aa").join(&self.m_buildTarget)
    }
}

#[allow(non_snake_case)]
#[derive(Debug, Deserialize)]
pub struct CatalogLocation {
    pub m_Keys: Vec<String>,
    pub m_InternalId: String,
    pub m_Provider: String,
    pub m_Dependencies: Vec<()>,
    pub m_ResourceType: AssemblyClass,
}

#[allow(non_snake_case)]
#[derive(Debug, Deserialize)]
pub struct AssemblyClass {
    pub m_AssemblyName: String,
    pub m_ClassName: String,
}

// archive:/CAB-asdf/CAB-asdf
// archive:/CAB-asdf/CAB-asdf.sharedAssets
#[derive(Debug, Clone, Copy)]
pub struct ArchivePath<'a> {
    pub bundle: &'a str,
    pub file: &'a str,
}
impl std::fmt::Display for ArchivePath<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "archive:/{}/{}", self.bundle, self.file)
    }
}
impl From<ArchivePath<'_>> for PathBuf {
    fn from(value: ArchivePath<'_>) -> Self {
        PathBuf::from(value.to_string())
    }
}

impl<'a> ArchivePath<'a> {
    pub fn new(bundle: &'a str, file: &'a str) -> Self {
        ArchivePath { bundle, file }
    }

    pub fn same(path: &'a str) -> Self {
        ArchivePath {
            bundle: path,
            file: path,
        }
    }

    /// Attempts to parse a path as `archive:/bundle/file`.
    /// Returns `None` if it doesn't match the format.
    pub fn try_parse(path: &Path) -> Result<Option<ArchivePath<'_>>, InvalidArchivePath> {
        fn parse_inner(inner: &Path) -> Option<ArchivePath<'_>> {
            let mut parts = inner.iter();
            let bundle = parts.next()?.to_str()?;
            let file = parts.next()?.to_str()?;
            Some(ArchivePath { bundle, file })
        }

        let Ok(inner) = path.strip_prefix("archive:") else {
            return Ok(None);
        };

        let value =
            parse_inner(inner).ok_or_else(|| InvalidArchivePath(inner.display().to_string()))?;
        Ok(Some(value))
    }
}

#[derive(Debug)]
pub struct InvalidArchivePath(String);
impl std::fmt::Display for InvalidArchivePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Invalid archive path: `{}`", self.0)
    }
}
impl std::error::Error for InvalidArchivePath {}
