//! The combined image sampler split, against the programs of real builds.
//!
//! Three sources cover the three shapes a module can have: Silksong's shaders, where
//! every texture is combined; the 6000 fixtures, which mix combined and separate ones
//! in one bundle; and Hollow Knight 1.5.78, which is the form the split produces and
//! must therefore come out untouched.
//!
//! What the split has to hold up is checked as properties, not as golden bytes: the
//! module still reads, nothing stays combined, every texture has a sampler, the two
//! halves are numbered the way the older builds number theirs, and running it twice
//! changes nothing the first run did not.

use std::io::Cursor;
use std::path::Path;

use anyhow::{Context, Result};
use rabex::UnityVersion;
use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig, FallbackUnityVersion};
use rabex::objects::ClassId;
use rabex::tpk::TpkTypeTreeBlob;
use rabex::typetree::typetree_cache::sync::TypeTreeCache;
use rabex_shader::vulkan::VulkanProgram;
use rabex_shader::{Platform, Shader, SubProgramView};
use rabex_shader_convert::spirv::{Resource, ResourceKind, resources};
use rabex_shader_convert::split_samplers::split_combined_samplers;

/// The fixtures mix both shapes in one bundle, and they are in the repository.
const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../rabex-shader/tests/fixtures/6000.3.27f2/shaders"
);
const SILKSONG: &str = "/home/jakob/.steamapps/Hollow Knight Silksong/Hollow Knight Silksong_Data/StreamingAssets/aa/StandaloneLinux64/shaders_assets_all.bundle";
const HOLLOW_KNIGHT_2020: &str =
    "/home/jakob/.steamapps/Hollow Knight-1.5.78.11833/hollow_knight_Data/resources.assets";

/// What one source's modules amounted to.
#[derive(Default)]
struct Counts {
    modules: usize,
    split: usize,
    untouched: usize,
}

#[test]
fn every_combined_sampler_is_split_and_numbered_like_the_older_builds() -> Result<()> {
    let sources = [
        (FIXTURES, "6000.3.27f2"),
        (SILKSONG, "6000.0.50f1"),
        (HOLLOW_KNIGHT_2020, "2020.2.2f1"),
    ];
    let mut any = false;
    for (path, version) in sources {
        let path = Path::new(path);
        if !path.exists() {
            continue;
        }
        any = true;
        let counts = check(path, &version.parse()?)?;
        println!(
            "{}: {} modules, {} split, {} untouched",
            path.file_name().unwrap_or_default().to_string_lossy(),
            counts.modules,
            counts.split,
            counts.untouched
        );
        assert!(
            counts.modules > 0,
            "{} had no Vulkan module",
            path.display()
        );
    }
    assert!(any, "none of the sources is on this machine");
    Ok(())
}

fn check(path: &Path, version: &UnityVersion) -> Result<Counts> {
    let tpk = TypeTreeCache::new(TpkTypeTreeBlob::embedded());
    let mut counts = Counts::default();
    for data in serialized_files(path, version)? {
        let Ok(file) = SerializedFile::from_reader_with_version(&mut Cursor::new(&data), version)
        else {
            continue;
        };
        for info in file.objects().filter(|o| o.m_ClassID == ClassId::Shader) {
            let Ok(shader) = file.read::<Shader>(info, &tpk, &mut Cursor::new(data.as_slice()))
            else {
                continue;
            };
            let name = &shader.m_ParsedForm.m_Name;
            for blob in shader.platform_blobs()? {
                if blob.platform != Platform::Vulkan {
                    continue;
                }
                for location in blob.entries()? {
                    let Ok(view) = SubProgramView::parse(blob.entry(location)?) else {
                        continue;
                    };
                    let program = VulkanProgram::parse(view.code).with_context(|| name.clone())?;
                    for stage in program.stages.iter().flatten() {
                        counts.modules += 1;
                        let original = smolv::decode(&stage.smolv)?;
                        let before = resources(&original)?;
                        let combined = count(&before, |r| {
                            matches!(r.kind, ResourceKind::Texture { combined: true, .. })
                        });

                        let mut words = original.clone();
                        let pairs =
                            split_combined_samplers(&mut words).with_context(|| name.clone())?;
                        assert_eq!(
                            pairs.len(),
                            combined,
                            "{name}: split {} of {combined}",
                            pairs.len()
                        );

                        if combined == 0 {
                            // the form the split produces passes through it
                            assert_eq!(
                                words, original,
                                "{name}: changed without a combined sampler"
                            );
                            counts.untouched += 1;
                            continue;
                        }
                        counts.split += 1;

                        let after = resources(&words)
                            .with_context(|| format!("{name}: the split module"))?;
                        assert_eq!(
                            count(&after, |r| matches!(
                                r.kind,
                                ResourceKind::Texture { combined: true, .. }
                            )),
                            0,
                            "{name}: a texture is still combined"
                        );
                        let textures: Vec<&Resource> = after
                            .iter()
                            .filter(|r| matches!(r.kind, ResourceKind::Texture { .. }))
                            .collect();
                        let samplers: Vec<&Resource> = after
                            .iter()
                            .filter(|r| matches!(r.kind, ResourceKind::Sampler))
                            .collect();
                        assert_eq!(
                            textures.len(),
                            samplers.len(),
                            "{name}: {} textures against {} samplers",
                            textures.len(),
                            samplers.len()
                        );

                        // nothing shares a binding, and nothing that was already there
                        // moved: a set holds buffers and images too, and the player
                        // binds all of them by these numbers
                        let mut seen: Vec<(u32, u32)> = Vec::new();
                        for resource in &after {
                            let slot = (resource.set, resource.binding);
                            assert!(
                                !seen.contains(&slot),
                                "{name}: set {} binding {} handed out twice",
                                slot.0,
                                slot.1
                            );
                            seen.push(slot);
                        }
                        for old in &before {
                            if matches!(old.kind, ResourceKind::Texture { combined: true, .. }) {
                                continue;
                            }
                            assert!(
                                after.iter().any(|r| r.id == old.id
                                    && r.set == old.set
                                    && r.binding == old.binding),
                                "{name}: {} moved from set {} binding {}",
                                old.name,
                                old.set,
                                old.binding
                            );
                        }

                        // the result is the form the split leaves alone
                        let mut again = words.clone();
                        let second = split_combined_samplers(&mut again)
                            .with_context(|| format!("{name}: split twice"))?;
                        assert!(second.is_empty(), "{name}: a second split found work");
                        assert_eq!(again, words, "{name}: a second split changed the module");

                        // and it survives the encoding the container stores it in
                        let reencoded = smolv::decode(&smolv::encode(&words)?)?;
                        assert_eq!(reencoded, words, "{name}: lost in SMOL-V");
                    }
                }
            }
        }
    }
    Ok(counts)
}

fn count(resources: &[Resource], kind: impl Fn(&Resource) -> bool) -> usize {
    resources.iter().filter(|r| kind(r)).count()
}

/// The serialized files of a bundle, or the file itself.
fn serialized_files(path: &Path, version: &UnityVersion) -> Result<Vec<Vec<u8>>> {
    let raw = std::fs::read(path)?;
    if !raw.starts_with(b"UnityFS") {
        return Ok(vec![raw]);
    }
    let config = ExtractionConfig {
        fallback_unity_version: FallbackUnityVersion::Exact(version.clone()),
        ..Default::default()
    };
    let reader = BundleFileReader::from_reader(Cursor::new(raw), &config)?;
    Ok(reader
        .serialized_files()
        .map(|entry| reader.read_at_entry(entry))
        .collect::<Result<_, _>>()?)
}
