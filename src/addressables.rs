use std::path::Path;

use serde_derive::Deserialize;

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

pub fn wrap_archive(cab: &str) -> String {
    format!("archive:/{cab}/{cab}")
}
pub fn unwrap_archive(path: &Path) -> Option<&str> {
    let path = path.strip_prefix("archive:").ok()?;

    let mut parts = path.iter();
    let first = parts.next()?.to_str()?;
    let second = parts.next()?.to_str()?.trim_end_matches(".sharedAssets"); // TODO: necessary? or not
    if first != second {
        return None;
    }
    Some(second)
}
