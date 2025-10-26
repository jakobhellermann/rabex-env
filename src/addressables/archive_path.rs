use std::path::{Path, PathBuf};

// archive:/CAB-asdf/CAB-asdf
// archive:/CAB-asdf/CAB-asdf.sharedAssets
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
            Some(ArchivePath::new(bundle, file))
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
