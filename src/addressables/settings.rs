use std::path::{Path, PathBuf};

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
