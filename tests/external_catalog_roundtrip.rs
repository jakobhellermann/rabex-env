mod external_test_data;

use std::path::Path;

use anyhow::{Context, Result, bail};
use rabex_env::addressables::catalog::AddressablesCatalog;

#[test]
fn external_catalog_exact_roundtrip() -> Result<()> {
    let home = std::env::home_dir().context("no home directory")?;
    for path in external_test_data::EXTERNAL_CATALOGS {
        let path = match path.strip_prefix("~/") {
            Some(rest) => home.join(rest),
            None => path.into(),
        };
        assert_roundtrip(&path)
            .with_context(|| format!("could not roundtrip {}", path.display()))?;
    }
    Ok(())
}

fn assert_roundtrip(path: &Path) -> Result<()> {
    let data = std::fs::read(path)?;
    let written = AddressablesCatalog::from_bytes(&data)?.to_binary()?;
    if let Some(offset) = data.iter().zip(&written).position(|(a, b)| a != b) {
        bail!("first difference at {offset:#x}");
    }
    assert_eq!(data.len(), written.len());
    Ok(())
}
