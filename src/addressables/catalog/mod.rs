use anyhow::Result;

mod binary;
mod json;

use std::io::Cursor;

pub use binary::*;

/// Parse an addressables catalog, either as `catalog.json` or `catalog.bin`
pub(crate) fn parse(data: &[u8]) -> Result<AddressablesCatalog> {
    let is_json = data
        .iter()
        .find(|b| !b.is_ascii_whitespace())
        .is_some_and(|&b| b == b'{');
    if is_json {
        json::parse(data)
    } else {
        Ok(AddressablesCatalog::from_reader(Cursor::new(data))?)
    }
}
