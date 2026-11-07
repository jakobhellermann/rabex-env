//! The Vulkan variants of a pass against the search they replace, which tried every
//! combination of one variant per stage: the same variants, in the same order, on random
//! passes, on the fixtures' shaders, and on the archives `RABEX_SHADER_ARCHIVES` names
//! (`path[@version];…`, a path a bundle, an archive or a directory of bundles; the version
//! for headers without one).

mod common;

use std::collections::BTreeSet;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rabex::UnityVersion;
use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig, FallbackUnityVersion};
use rabex::objects::ClassId;
use rabex::tpk::TpkTypeTreeBlob;
use rabex::typetree::typetree_cache::sync::TypeTreeCache;

type TpkTrees = TypeTreeCache<TpkTypeTreeBlob>;
use rabex_shader::variants::Stage;
use rabex_shader::{Platform, Shader};
use rabex_shader_convert::target::vulkan::TRANSLATION_ORDER;
use rabex_shader_convert::target::vulkan::shader::{Combination, combinations};

/// The search as it was: every combination of one variant per stage, kept where each
/// chosen variant is the strictly largest within the combination's keywords, the first of
/// each keyword set, its stages chosen anew per stage.
fn reference(variants: &[(Stage, usize, &[String])]) -> Vec<Combination> {
    let tiers: BTreeSet<usize> = variants.iter().map(|v| v.1).collect();
    let mut out = Vec::new();
    for tier in tiers {
        let in_tier = || (0..variants.len()).filter(move |&i| variants[i].1 == tier);
        let mut stages = Vec::new();
        for i in in_tier() {
            if !stages.contains(&variants[i].0) {
                stages.push(variants[i].0);
            }
        }
        let mut combinations: Vec<Vec<usize>> = vec![Vec::new()];
        for stage in stages {
            combinations = combinations
                .into_iter()
                .flat_map(|combination| {
                    in_tier().filter(|&i| variants[i].0 == stage).map(move |i| {
                        let mut next = combination.clone();
                        next.push(i);
                        next
                    })
                })
                .collect();
        }
        let mut seen = BTreeSet::new();
        for combination in combinations {
            let keywords: Vec<String> = combination
                .iter()
                .flat_map(|&i| variants[i].2.iter().cloned())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            let fits = |i: usize| variants[i].2.iter().all(|k| keywords.contains(k));
            let exact = combination.iter().all(|&chosen| {
                in_tier()
                    .filter(|&i| variants[i].0 == variants[chosen].0 && fits(i))
                    .all(|i| i == chosen || variants[i].2.len() < variants[chosen].2.len())
            });
            if exact && seen.insert(keywords.clone()) {
                let mut picked = combination.clone();
                picked.sort_by_key(|&i| TRANSLATION_ORDER.iter().position(|&o| o == variants[i].0));
                out.push(Combination {
                    tier,
                    keywords,
                    stages: picked,
                });
            }
        }
    }
    out
}

fn same(variants: &[(Stage, usize, &[String])], what: &str) {
    assert_eq!(combinations(variants), reference(variants), "{what}");
}

/// Random passes: a few keywords, shared and stage-only ones, variants per stage drawn as
/// keyword subsets — duplicates, overlapping sets, several tiers and stages included.
#[test]
fn random_passes_match_the_search() {
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut next = |n: u64| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state % n
    };
    let names: Vec<String> = (0..7).map(|i| format!("K{i}")).collect();
    let stages = [Stage::Vertex, Stage::Fragment, Stage::Geometry];
    for case in 0..3000 {
        let mut keyword_sets: Vec<(Stage, usize, Vec<String>)> = Vec::new();
        let stage_count = 1 + next(3) as usize;
        let tiers = 1 + next(2) as usize;
        for tier in 0..tiers {
            for &stage in &stages[..stage_count] {
                for _ in 0..1 + next(6) {
                    let keywords: Vec<String> =
                        names.iter().filter(|_| next(3) == 0).cloned().collect();
                    keyword_sets.push((stage, tier, keywords));
                }
            }
        }
        // stages and tiers interleaved in the input, as blobs list them
        for i in (1..keyword_sets.len()).rev() {
            let j = next(i as u64 + 1) as usize;
            keyword_sets.swap(i, j);
        }
        let variants: Vec<(Stage, usize, &[String])> = keyword_sets
            .iter()
            .map(|(stage, tier, keywords)| (*stage, *tier, keywords.as_slice()))
            .collect();
        same(&variants, &format!("case {case}: {keyword_sets:?}"));
    }
}

/// The D3D11 passes of `shaders`: each pass's variants, checked.
fn check_shaders(shaders: &[Shader], what: &str) -> Result<usize> {
    let mut passes = 0;
    // the Vulkan target builds Unity 2022+ shaders, whose variants carry keyword names
    for shader in shaders
        .iter()
        .filter(|s| s.m_ParsedForm.m_KeywordNames.is_some())
    {
        let blobs = shader.platform_blobs()?;
        let Some(d3d11) = blobs.iter().find(|b| b.platform == Platform::D3D11) else {
            continue;
        };
        for pass in shader
            .m_ParsedForm
            .m_SubShaders
            .iter()
            .flat_map(|s| &s.m_Passes)
        {
            let variants = shader.pass_variants(pass, d3d11)?;
            let keys: Vec<(Stage, usize, &[String])> = variants
                .iter()
                .map(|v| (v.stage, v.tier, v.keywords.as_slice()))
                .collect();
            same(
                &keys,
                &format!("{what}: {} {}", shader.m_ParsedForm.m_Name, pass.m_Name),
            );
            passes += 1;
        }
    }
    Ok(passes)
}

#[test]
fn fixture_shaders_match_the_search() -> Result<()> {
    let mut passes = 0;
    for dir in common::fixture_dirs()? {
        let path = dir.join("StandaloneWindows64/shaders");
        if path.is_file() {
            passes += check_shaders(&common::shaders(&path)?, &path.display().to_string())?;
        }
    }
    assert!(passes > 0, "no D3D11 passes in the fixtures");
    Ok(())
}

/// Every shader of an archive's serialized files, read with the trees they embed or, for
/// files without, the type tree package's.
fn archive_shaders(
    path: &Path,
    version: Option<&UnityVersion>,
    tpk: &TpkTrees,
) -> Result<Vec<Shader>> {
    let mut config = ExtractionConfig::default();
    if let Some(version) = version {
        config.fallback_unity_version = FallbackUnityVersion::Exact(version.clone());
    }
    let reader = BundleFileReader::from_reader(Cursor::new(std::fs::read(path)?), &config)?;
    let mut shaders = Vec::new();
    for entry in reader.serialized_files() {
        let data = reader.read_entry(entry)?;
        let mut cursor = Cursor::new(data.as_ref());
        let file = match version {
            Some(version) => SerializedFile::from_reader_with_version(&mut cursor, version)?,
            None => SerializedFile::from_reader(&mut cursor)?,
        };
        for info in file.objects().filter(|o| o.m_ClassID == ClassId::Shader) {
            let shader: Shader = file
                .read(info, tpk, &mut Cursor::new(data.as_ref()))
                .with_context(|| format!("{} shader {}", path.display(), info.m_PathID))?;
            shaders.push(shader);
        }
    }
    Ok(shaders)
}

fn archives(path: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if !path.is_dir() {
        out.push(path.to_owned());
        return Ok(());
    }
    for entry in std::fs::read_dir(path)? {
        let path = entry?.path();
        if path.is_dir() {
            archives(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "bundle") {
            out.push(path);
        }
    }
    Ok(())
}

#[test]
fn named_archives_match_the_search() -> Result<()> {
    let Ok(list) = std::env::var("RABEX_SHADER_ARCHIVES") else {
        return Ok(());
    };
    let tpk = TpkTrees::new(TpkTypeTreeBlob::embedded());
    for spec in list.split(';').filter(|s| !s.is_empty()) {
        let (path, version) = spec
            .split_once('@')
            .map_or((spec, None), |(p, v)| (p, Some(v)));
        let version: Option<UnityVersion> = version.map(str::parse).transpose()?;
        let mut paths = Vec::new();
        archives(Path::new(path), &mut paths)?;
        let mut passes = 0;
        for path in paths {
            let shaders = archive_shaders(&path, version.as_ref(), &tpk)?;
            passes += check_shaders(&shaders, &path.display().to_string())?;
        }
        eprintln!("{spec}: {passes} passes match");
    }
    Ok(())
}
