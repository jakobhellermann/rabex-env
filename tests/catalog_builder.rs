use anyhow::Result;
use rabex_env::addressables::catalog::{
    AddressablesCatalog, AssemblyClass, AssetBundleRequestOptions, CatalogBuilder, assemblies,
};

#[test]
fn built_catalog_roundtrips() -> Result<()> {
    let mut builder = CatalogBuilder::default();
    let scripts = builder.add_bundle(
        "scripts.bundle",
        "/bundles/scripts.bundle",
        AssetBundleRequestOptions::new("scripts"),
    );
    let enemies = builder.add_bundle(
        "enemies.bundle",
        "/bundles/enemies.bundle",
        AssetBundleRequestOptions::new("enemies"),
    );
    builder.add_key("monoscripts", scripts);
    let game_object = AssemblyClass::new(assemblies::CORE_MODULE, "UnityEngine.GameObject");
    let crawler = builder.add_asset(
        "crawler",
        "a/crawler.prefab",
        game_object,
        vec![enemies, scripts],
    );
    let catalog = builder.build("test", "00000000000000000000000000000000");

    let read = AddressablesCatalog::from_bytes(&catalog.to_binary()?)?;
    assert_eq!(read.locations, catalog.locations);
    let keys: Vec<(&str, &[usize])> = read
        .resources
        .iter()
        .map(|(key, locations)| (key.as_str(), locations.as_slice()))
        .collect();
    assert_eq!(
        keys,
        [
            ("scripts.bundle", &[scripts][..]),
            ("enemies.bundle", &[enemies][..]),
            ("monoscripts", &[scripts][..]),
            ("crawler", &[crawler][..]),
        ]
    );
    Ok(())
}
