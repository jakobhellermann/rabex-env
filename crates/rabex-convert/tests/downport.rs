//! The shader downport against the fixture pair: the same shaders built by the 6000 and
//! 2020.2 editors, so a downported 6000 shader can be compared with Unity's own 2020.2
//! build of it.

use std::io::Cursor;

use anyhow::{Context, Result, bail, ensure};
use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig};
use rabex::objects::ClassId;
use rabex::typetree::NullTypeTreeProvider;
use rabex_shader::Platform;
use rabex_shader::types::{
    SerializedPass, SerializedProgram, SerializedSubProgram, Shader as RabexShader, Shader,
};

const NEW: &str = "../rabex-shader/tests/fixtures/6000.3.27f2";
const OLD: &str = "../rabex-shader/tests/fixtures/2020.2.2f1";
/// The versions the fixture bundles were built with: the new and the old grammar form.
fn new_form() -> rabex::UnityVersion {
    "6000.3.27f2".parse().unwrap()
}

fn old_form() -> rabex::UnityVersion {
    "2020.2.2f1".parse().unwrap()
}

/// The Shader class tree of `version`, straight from the type tree package: what the
/// rules read the target form from.
fn shader_tree(version: &rabex::UnityVersion) -> rabex::typetree::TypeTreeNode {
    let tpk = rabex::typetree::typetree_cache::sync::TypeTreeCache::new(
        rabex::tpk::TpkTypeTreeBlob::embedded(),
    );
    use rabex::typetree::TypeTreeProvider;
    tpk.get_typetree_node(rabex::objects::ClassId::Shader, version)
        .expect("the package carries the Shader tree")
        .into_owned()
}

fn bundle_shaders(dir: &str) -> Result<Vec<RabexShader>> {
    let path = std::path::Path::new(dir).join("shaders");
    let bundle = BundleFileReader::from_reader(
        Cursor::new(std::fs::read(&path).with_context(|| path.display().to_string())?),
        &ExtractionConfig::default(),
    )?;
    let entry = bundle.main_serializedfile().context("no serialized file")?;
    let data = bundle.read_at_entry(entry)?;
    let file = SerializedFile::from_reader(&mut Cursor::new(data.as_slice()))?;
    let mut shaders = Vec::new();
    for info in file.objects().filter(|o| o.m_ClassID == ClassId::Shader) {
        shaders.push(file.read(
            info,
            NullTypeTreeProvider,
            &mut Cursor::new(data.as_slice()),
        )?);
    }
    ensure!(!shaders.is_empty(), "{dir}: no shaders");
    Ok(shaders)
}

/// The 2020.2 keyword set a sub program carries, resolved through its pass table.
fn keywords_of(pass: &SerializedPass, sub: &SerializedSubProgram) -> Vec<String> {
    let mut all = Vec::new();
    for &slot in sub.m_GlobalKeywordIndices.iter().flatten() {
        all.push(resolve(pass, slot as usize));
    }
    for &slot in sub.m_LocalKeywordIndices.iter().flatten() {
        all.push(resolve(pass, slot as usize));
    }
    all.sort();
    all
}

fn resolve(pass: &SerializedPass, slot: usize) -> String {
    pass.m_NameIndices
        .iter()
        .find(|(_, index)| **index as usize == slot)
        .map(|(name, _)| name.clone())
        .unwrap_or_else(|| panic!("slot {slot} missing from the pass table"))
}

/// Constants the 2020.2 build lists and the newer compiler's parameter blob does not,
/// so no conversion can produce them. The render comparison of the fixtures shows the
/// OpenGL output matching Unity's own pixel for pixel without them, which is what says
/// the player fills them regardless.
const ABSENT_FROM_THE_NEWER_BLOB: &[&str] = &[
    "$Globals.unity_ObjectToWorld(0 4x4, matrix)",
    "$Globals.unity_MatrixVP(0 4x4, matrix)",
];

#[test]
fn downported_shaders_match_the_2020_2_build() -> Result<()> {
    let new = bundle_shaders(NEW)?;
    let old = bundle_shaders(OLD)?;
    let old_by_name: std::collections::BTreeMap<_, _> = old
        .iter()
        .map(|shader| (shader.m_ParsedForm.m_Name.clone(), shader))
        .collect();
    let chunk_size = 2 * 1024 * 1024;

    let mut checked = 0;
    for shader in &new {
        let name = &shader.m_ParsedForm.m_Name;
        let expected = old_by_name
            .get(name)
            .with_context(|| format!("the 2020.2 build lacks {name}"))?;
        let downported =
            rabex_convert::shader::convert(shader, &shader_tree(&old_form()), chunk_size)
                .with_context(|| name.clone())?;

        // platforms: the same set
        let platforms = |shader: &Shader| -> Result<Vec<Platform>> {
            shader
                .platform_blobs()?
                .iter()
                .map(|b| Ok(b.platform))
                .collect()
        };
        let want = platforms(expected)?;
        ensure!(
            platforms(&downported)? == want,
            "{name}: platforms differ from the 2020.2 build"
        );

        // the 2020.2 build and the downport must agree, variant by variant

        ensure!(
            passes_of(&downported).len() == passes_of(expected).len(),
            "{name}: pass count differs"
        );
        for (mine, theirs) in passes_of(&downported).into_iter().zip(passes_of(expected)) {
            compare_pass(name, mine, theirs)?;
        }
        compare_entries(name, &downported, expected)?;
        checked += 1;
    }
    ensure!(checked > 0, "no shaders checked");
    Ok(())
}

/// The inline parameter tables of one entry, reduced to what must survive the downport:
/// the program type, both keyword lists, every constant by name and shape grouped by
/// scope, and the bindings by kind and name. Unity 2020.2 splits a combined GL
/// program's `$Globals` per stage, which the 2022+ parameter blobs already merged into
/// one scope, and its stage Globals blocks carry a hash their compiler computed, so
/// scopes with the same name merge for the comparison and Globals names compare by
/// prefix. Offsets and sizes don't carry across versions: the carried code decides them.
#[derive(Debug)]
struct EntryTables {
    program_type: u32,
    globals: Vec<String>,
    locals: Vec<String>,
    /// Every constant buffer in order, as `(name, size, constants and structs)`. Not
    /// keyed by name: a GL entry carries one `$Globals` per stage, and keying would
    /// fold them into one and hide a merge that drops a stage's constants.
    scopes: Vec<(String, u32, Vec<String>)>,
    bindings: Vec<String>,
}

fn entry_tables(bytes: &[u8]) -> Result<EntryTables> {
    let view = rabex_shader::SubProgramView::parse(bytes)?;
    let (locals, parameters) = match &view.layout {
        rabex_shader::SubProgramViewLayout::V201806140 {
            local_keywords,
            parameters,
        } => (local_keywords.clone(), parameters),
        _ => bail!("entry lacks the 2020.2 layout"),
    };
    let scope_name = |name: &str| -> String {
        let trimmed = name.trim_end_matches(|c: char| c.is_ascii_digit());
        match trimmed {
            "VGlobals" | "PGlobals" | "GGlobals" => trimmed.to_string(),
            _ => name.to_string(),
        }
    };
    let mut scopes: Vec<(String, u32, Vec<String>)> = Vec::new();
    for scope in &parameters.constant_buffers {
        let mut entry: Vec<String> = Vec::new();
        for constant in &scope.constants {
            entry.push(format!(
                "{}({} {}x{}{}{})",
                constant.name,
                constant.data_type,
                constant.rows,
                constant.columns.unwrap_or(0),
                if constant.is_matrix { ", matrix" } else { "" },
                if constant.array_size > 0 {
                    format!(" [{}]", constant.array_size)
                } else {
                    String::new()
                }
            ));
        }
        for s in &scope.structs {
            entry.push(format!("struct {}", s.name));
        }
        entry.sort();
        scopes.push((scope_name(&scope.name), scope.size, entry));
    }
    let mut bindings: Vec<_> = parameters.bindings.iter().map(binding_key).collect();
    bindings.sort();
    Ok(EntryTables {
        program_type: view.program_type,
        globals: view.keywords.clone(),
        locals,
        scopes,
        bindings,
    })
}

fn compare_entries(name: &str, mine: &Shader, theirs: &Shader) -> Result<()> {
    let mut my_blobs = Vec::new();
    for blob in mine.platform_blobs()? {
        my_blobs.push((blob.platform, blob));
    }
    let mut their_blobs = Vec::new();
    for blob in theirs.platform_blobs()? {
        their_blobs.push((blob.platform, blob));
    }
    ensure!(
        my_blobs.len() == their_blobs.len(),
        "platform count differs"
    );
    for (platform, my_blob) in &my_blobs {
        let their_blob = &their_blobs
            .iter()
            .find(|(p, _)| p == platform)
            .with_context(|| format!("{platform:?} missing"))?
            .1;
        let mut my_subs = Vec::new();
        for sub in passes_of(mine)
            .iter()
            .flat_map(|pass| pass.programs())
            .flat_map(|(_, program)| &program.m_SubPrograms)
        {
            if platform.runs(u32::try_from(sub.m_GpuProgramType)?)? {
                my_subs.push(sub);
            }
        }
        let mut their_subs = Vec::new();
        for sub in passes_of(theirs)
            .iter()
            .flat_map(|pass| pass.programs())
            .flat_map(|(_, program)| &program.m_SubPrograms)
        {
            if platform.runs(u32::try_from(sub.m_GpuProgramType)?)? {
                their_subs.push(sub);
            }
        }
        ensure!(
            my_subs.len() == their_subs.len(),
            "{platform:?}: {} sub programs, the 2020.2 build has {}",
            my_subs.len(),
            their_subs.len()
        );
        for (my_sub, their_sub) in my_subs.iter().zip(&their_subs) {
            let mine = entry_tables(blob_entry(my_blob, my_sub.m_BlobIndex)?)?;
            let theirs = entry_tables(blob_entry(their_blob, their_sub.m_BlobIndex)?)?;
            let label = format!(
                "{name} {platform:?} blob {} vs {}",
                my_sub.m_BlobIndex, their_sub.m_BlobIndex
            );
            // the keyword lists are sets to the player; the versions list them in
            // their own order
            let sorted = |list: &[String]| {
                let mut sorted = list.to_vec();
                sorted.sort();
                sorted
            };
            ensure!(
                mine.program_type == theirs.program_type
                    && sorted(&mine.globals) == sorted(&theirs.globals)
                    && sorted(&mine.locals) == sorted(&theirs.locals),
                "{label}: keywords or program type differ: {:?} vs {:?}",
                (&mine.globals, &mine.locals),
                (&theirs.globals, &theirs.locals)
            );
            fn count(list: &[String]) -> std::collections::BTreeMap<&String, usize> {
                let mut counts: std::collections::BTreeMap<&String, usize> = Default::default();
                for binding in list {
                    *counts.entry(binding).or_default() += 1;
                }
                counts
            }
            let (my_bindings, their_bindings) = (count(&mine.bindings), count(&theirs.bindings));
            let extra: Vec<_> = my_bindings
                .iter()
                .filter(|(k, n)| their_bindings.get(*k).is_none_or(|t| t < *n))
                .map(|(k, _)| k)
                .collect();
            ensure!(
                extra.is_empty(),
                "{label}: bindings {extra:?} beyond the 2020.2 build"
            );
            let lost: Vec<_> = their_bindings
                .iter()
                .filter(|(k, n)| my_bindings.get(*k).is_none_or(|m| m < *n))
                .map(|(k, _)| k)
                .collect();
            // The newer compiler emits one inline sampler state where the 2020.2 one
            // emits two; the blob we convert from has only the one, so the second
            // cannot be produced. The fixture renders its other passes identically,
            // which is what says the state it does declare is the one in use.
            let lost: Vec<_> = lost
                .into_iter()
                .filter(|name| name.as_str() != "sampler")
                .collect();
            ensure!(
                lost.is_empty(),
                "{label}: lacks the 2020.2 build's bindings {lost:?}"
            );
            // The two builds group the constants differently: a GL entry of the newer
            // compiler holds one `$Globals` with every stage's constants, the older one
            // holds a `$Globals` per stage. The grouping is the compilers', so what has
            // to match is which constants an entry carries, not how they are split.
            let constants = |scopes: &[(String, u32, Vec<String>)]| {
                let mut all: Vec<String> = scopes
                    .iter()
                    .flat_map(|(name, _, constants)| {
                        constants.iter().map(move |c| format!("{name}.{c}"))
                    })
                    .collect();
                all.sort();
                all
            };
            let (mine, theirs) = (constants(&mine.scopes), constants(&theirs.scopes));
            let missing: Vec<_> = theirs
                .iter()
                .filter(|c| !mine.contains(c))
                .filter(|c| !ABSENT_FROM_THE_NEWER_BLOB.contains(&c.as_str()))
                .collect();
            let extra: Vec<_> = mine.iter().filter(|c| !theirs.contains(c)).collect();
            ensure!(
                missing.is_empty() && extra.is_empty(),
                "{label}: constants {extra:?} beyond the 2020.2 build, {missing:?} missing"
            );
        }
    }
    Ok(())
}

/// The stage Globals block names carry a hash their compiler computed; the versions
/// differ in it.
fn globals_prefix(name: &str) -> String {
    let trimmed = name.trim_end_matches(|c: char| c.is_ascii_digit());
    match trimmed {
        "VGlobals" | "PGlobals" | "GGlobals" => trimmed.to_string(),
        _ => name.to_string(),
    }
}

fn binding_key(binding: &rabex_shader::Binding) -> String {
    let name = |name: &str| {
        let trimmed = name.trim_end_matches(|c: char| c.is_ascii_digit());
        match trimmed {
            "VGlobals" | "PGlobals" | "GGlobals" => trimmed.to_string(),
            _ => name.to_string(),
        }
    };
    match binding {
        rabex_shader::Binding::Texture { name: n, .. } => format!("texture {}", name(n)),
        rabex_shader::Binding::ConstantBuffer { name: n, .. } => format!("cb {}", name(n)),
        rabex_shader::Binding::Buffer { name: n, .. } => format!("buffer {}", name(n)),
        rabex_shader::Binding::Uav { name: n, .. } => format!("uav {}", name(n)),
        rabex_shader::Binding::Sampler { .. } => "sampler".to_string(),
    }
}

fn blob_entry(blob: &rabex_shader::PlatformBlob, index: u32) -> Result<&[u8]> {
    let location = *blob
        .entries()?
        .get(index as usize)
        .with_context(|| format!("blob index {index} out of range"))?;
    blob.entry(location)
}

fn passes_of(shader: &Shader) -> Vec<&SerializedPass> {
    shader
        .m_ParsedForm
        .m_SubShaders
        .iter()
        .flat_map(|s| &s.m_Passes)
        .collect()
}

fn compare_pass(name: &str, mine: &SerializedPass, theirs: &SerializedPass) -> Result<()> {
    // the name tables cover the same names; the stage Globals blocks carry a hash of
    // their members that the shader compiler of each version computes differently, so
    // their names compare by prefix
    let keys = |pass: &SerializedPass| {
        let mut names: Vec<_> = pass
            .m_NameIndices
            .keys()
            .map(|name| {
                let trimmed = name.trim_end_matches(|c: char| c.is_ascii_digit());
                match trimmed {
                    "VGlobals" | "PGlobals" | "GGlobals" => trimmed.to_string(),
                    _ => name.clone(),
                }
            })
            .collect::<Vec<_>>();
        names.sort();
        names.dedup();
        names
    };
    if keys(mine) != keys(theirs) {
        let missing = keys(theirs)
            .iter()
            .filter(|k| !keys(mine).contains(k))
            .cloned()
            .collect::<Vec<_>>();
        let extra = keys(mine)
            .iter()
            .filter(|k| !keys(theirs).contains(k))
            .cloned()
            .collect::<Vec<_>>();
        bail!("{name}: name tables differ; missing {missing:?} extra {extra:?}");
    }

    for ((stage, my_program), (_, their_program)) in
        mine.programs().iter().zip(theirs.programs().iter())
    {
        compare_stage(name, *stage, my_program, their_program, mine, theirs)?;
    }
    Ok(())
}

fn compare_stage(
    name: &str,
    stage: rabex_shader::Stage,
    mine: &SerializedProgram,
    theirs: &SerializedProgram,
    my_pass: &SerializedPass,
    their_pass: &SerializedPass,
) -> Result<()> {
    ensure!(
        mine.m_SubPrograms.len() == theirs.m_SubPrograms.len(),
        "{name} {stage:?}: {} sub programs, the 2020.2 build has {}",
        mine.m_SubPrograms.len(),
        theirs.m_SubPrograms.len()
    );
    for (index, (my_sub, their_sub)) in mine
        .m_SubPrograms
        .iter()
        .zip(&theirs.m_SubPrograms)
        .enumerate()
    {
        let mine_kws = keywords_of(my_pass, my_sub);
        let their_kws = keywords_of(their_pass, their_sub);
        ensure!(
            mine_kws == their_kws,
            "{name} {stage:?} sub {index}: keywords {mine_kws:?}, the 2020.2 build has {their_kws:?}"
        );
        ensure!(
            my_sub.m_GpuProgramType == their_sub.m_GpuProgramType,
            "{name} {stage:?} sub {index}: gpu type differs"
        );
        compare_flat(name, stage, index, my_sub, their_sub, my_pass, their_pass)?;
    }
    Ok(())
}

fn compare_flat(
    name: &str,
    stage: rabex_shader::Stage,
    index: usize,
    mine: &SerializedSubProgram,
    theirs: &SerializedSubProgram,
    my_pass: &SerializedPass,
    their_pass: &SerializedPass,
) -> Result<()> {
    let textures = |sub: &SerializedSubProgram, pass: &SerializedPass| {
        let mut names: Vec<_> = sub
            .m_TextureParams
            .iter()
            .flatten()
            .map(|t| resolve(pass, t.m_NameIndex as usize))
            .collect();
        names.sort();
        names
    };
    ensure!(
        textures(mine, my_pass) == textures(theirs, their_pass),
        "{name} {stage:?} sub {index}: textures {:?}, the 2020.2 build has {:?}",
        textures(mine, my_pass),
        textures(theirs, their_pass)
    );

    // the vertex channel table and source map carry the source entry's bindings
    let channels = |sub: &SerializedSubProgram| {
        (
            sub.m_Channels.m_SourceMap,
            sub.m_Channels
                .m_Channels
                .iter()
                .map(|c| (c.source, c.target))
                .collect::<Vec<_>>(),
        )
    };
    ensure!(
        channels(mine) == channels(theirs),
        "{name} {stage:?} sub {index}: channels {:?} vs the 2020.2 build's {:?}",
        channels(mine),
        channels(theirs)
    );

    // the flat constant buffers mirror the entry's scope tables: names and constant
    // shapes must line up, offsets and sizes stay free for the same reasons
    // the flat constant buffers merge per name: the 2020.2 build splits a combined GL
    // program's $Globals per stage like its entry tables
    let buffers = |sub: &SerializedSubProgram,
                   pass: &SerializedPass|
     -> std::collections::BTreeMap<String, Vec<String>> {
        let mut merged: std::collections::BTreeMap<String, Vec<String>> = Default::default();
        for cb in sub.m_ConstantBuffers.iter().flatten() {
            let name = globals_prefix(&resolve(pass, cb.m_NameIndex as usize));
            let constants = merged.entry(name).or_default();
            constants.extend(cb.m_VectorParams.iter().map(|v| {
                format!(
                    "{}({} {}x{})",
                    resolve(pass, v.m_NameIndex as usize),
                    v.m_Type,
                    v.m_Dim,
                    v.m_Index
                )
            }));
            constants.extend(cb.m_MatrixParams.iter().map(|m| {
                format!(
                    "{}({} rows {})",
                    resolve(pass, m.m_NameIndex as usize),
                    m.m_Type,
                    m.m_RowCount
                )
            }));
            constants.extend(
                cb.m_StructParams
                    .iter()
                    .map(|s| format!("struct {}", resolve(pass, s.m_NameIndex as usize))),
            );
        }
        for constants in merged.values_mut() {
            constants.sort();
        }
        merged
    };
    let (my_buffers, their_buffers) = (buffers(mine, my_pass), buffers(theirs, their_pass));
    ensure!(
        my_buffers.keys().eq(their_buffers.keys()),
        "{name} {stage:?} sub {index}: flat constant buffers {:?} vs the 2020.2 build's {:?}",
        my_buffers.keys().collect::<Vec<_>>(),
        their_buffers.keys().collect::<Vec<_>>()
    );
    for (buffer, my_constants) in &my_buffers {
        let their_constants = &their_buffers[buffer];
        let missing: Vec<_> = my_constants
            .iter()
            .filter(|c| !their_constants.contains(c))
            .collect();
        ensure!(
            missing.is_empty(),
            "{name} {stage:?} sub {index}: flat constant buffer {buffer:?} has {missing:?} beyond the 2020.2 build"
        );
    }
    Ok(())
}

/// The 2022+ editors pad their keyword name lists with the stereo set, which no variant
/// uses; the upport's rebuilt list leaves it out.
const PADDING: [&str; 4] = [
    "STEREO_INSTANCING_ON",
    "UNITY_SINGLE_PASS_STEREO",
    "STEREO_MULTIVIEW_ON",
    "STEREO_CUBEMAP_RENDER_ON",
];

#[test]
fn the_uport_restores_what_the_downport_carried() -> Result<()> {
    let new = bundle_shaders(NEW)?;
    let chunk_size = 2 * 1024 * 1024;

    let mut checked = 0;
    for shader in &new {
        let name = &shader.m_ParsedForm.m_Name;
        let downported =
            rabex_convert::shader::convert(shader, &shader_tree(&old_form()), chunk_size)?;
        let upported =
            rabex_convert::shader::convert(&downported, &shader_tree(&new_form()), chunk_size)?;

        // the keyword registry, minus the padding the 2022+ editors mix in
        // the keyword name list's order only decides indices; the variants carry
        // their own index lists against it
        let names = |shader: &RabexShader| -> Vec<String> {
            let mut names: Vec<String> = shader
                .m_ParsedForm
                .m_KeywordNames
                .iter()
                .flatten()
                .filter(|n| !PADDING.contains(&n.as_str()))
                .cloned()
                .collect();
            names.sort();
            names
        };
        ensure!(
            names(&upported) == names(shader),
            "{name}: keyword names {:?} vs the original's {:?}",
            names(&upported),
            names(shader)
        );
        let flags = |shader: &RabexShader| -> Vec<(String, u8)> {
            let mut pairs: Vec<(String, u8)> = shader
                .m_ParsedForm
                .m_KeywordNames
                .iter()
                .flatten()
                .zip(shader.m_ParsedForm.m_KeywordFlags.iter().flatten())
                .filter(|(n, _)| !PADDING.contains(&n.as_str()))
                .map(|(n, f)| (n.clone(), *f))
                .collect();
            pairs.sort();
            pairs
        };
        ensure!(
            flags(&upported) == flags(shader),
            "{name}: keyword flags {:?} vs the original's {:?}",
            flags(&upported),
            flags(shader)
        );

        // the tier slots mirror the original: every hardware tier listed, the
        // 2020.2 build's tier the filled one, empty stages without any
        let tiers = |shader: &RabexShader| -> Vec<Vec<usize>> {
            shader
                .m_ParsedForm
                .m_SubShaders
                .iter()
                .flat_map(|s| &s.m_Passes)
                .flat_map(|pass| pass.programs())
                .map(|(_, program)| {
                    program
                        .m_PlayerSubPrograms
                        .as_ref()
                        .map(|t| t.iter().map(|v| v.len()).collect())
                        .unwrap_or_default()
                })
                .collect()
        };
        ensure!(
            tiers(&upported) == tiers(shader),
            "{name}: tier slots {:?} vs the original's {:?}",
            tiers(&upported),
            tiers(shader)
        );

        // every variant: stage, keywords, requirements, code and merged parameters
        let variants = |shader: &RabexShader| -> Result<Vec<_>> {
            let mut all = Vec::new();
            for pass in passes_of(shader) {
                for blob in shader.platform_blobs()? {
                    for variant in shader.pass_variants(pass, &blob)? {
                        all.push((
                            blob.platform,
                            variant.stage,
                            variant.keywords.clone(),
                            variant.requirements,
                            variant.program.code.to_vec(),
                            variant.parameters.clone(),
                        ));
                    }
                }
            }
            Ok(all)
        };
        let mine = variants(&upported)?;
        let theirs = variants(shader)?;
        ensure!(
            mine.len() == theirs.len(),
            "{name}: {} variants, the original has {}",
            mine.len(),
            theirs.len()
        );
        for (mine, theirs) in mine.iter().zip(&theirs) {
            let mut mine_keywords = mine.2.clone();
            mine_keywords.sort();
            let mut their_keywords = theirs.2.clone();
            their_keywords.sort();
            ensure!(
                mine.0 == theirs.0
                    && mine.1 == theirs.1
                    && mine_keywords == their_keywords
                    && mine.3 == theirs.3,
                "{name}: variant {mine_keywords:?} differs from the original's {their_keywords:?}"
            );
            // The programs of every platform carry through untouched, except Vulkan's:
            // those are rewritten on the way down, because the older player binds a
            // texture and its sampler separately and cannot feed a combined one. That
            // is a translation, not a transport, so the cycle does not restore the
            // bytes — what it has to restore is the program's shape.
            match mine.0 {
                rabex_shader::Platform::Vulkan => {
                    let stages = |code: &[u8]| -> Result<usize> {
                        Ok(rabex_shader::vulkan::VulkanProgram::parse(code)?
                            .stages
                            .iter()
                            .flatten()
                            .count())
                    };
                    ensure!(
                        stages(&mine.4)? == stages(&theirs.4)?,
                        "{name}: variant {mine_keywords:?}: the Vulkan program lost a stage"
                    );
                }
                _ => ensure!(
                    mine.4 == theirs.4,
                    "{name}: variant {mine_keywords:?}: code differs"
                ),
            }
            match mine.0 {
                // the texture bindings carry the split's numbering, which the way back
                // does not undo; the rest of the parameters must match exactly
                rabex_shader::Platform::Vulkan => {
                    let bindings = |p: &rabex_shader::ProgramParameters| {
                        p.bindings.iter().map(without_the_split).collect::<Vec<_>>()
                    };
                    ensure!(
                        bindings(&mine.5) == bindings(&theirs.5),
                        "{name}: variant {mine_keywords:?}: bindings {:?} vs the original's {:?}",
                        bindings(&mine.5),
                        bindings(&theirs.5)
                    );
                    let mut mine = mine.5.clone();
                    let mut theirs = theirs.5.clone();
                    mine.bindings.clear();
                    theirs.bindings.clear();
                    compare_parameters(name, &mine_keywords, &mine, &theirs)?;
                }
                _ => compare_parameters(name, &mine_keywords, &mine.5, &theirs.5)?,
            }
        }
        checked += 1;
    }
    ensure!(checked > 0, "no shaders checked");
    Ok(())
}

/// A texture binding as it is after the Vulkan translation and before it: the split
/// gives a texture its own sampler and a binding of its own, so those two numbers are
/// what the translation changes on purpose. Everything else about the binding stays.
fn without_the_split(binding: &rabex_shader::Binding) -> String {
    match binding {
        rabex_shader::Binding::Texture {
            name,
            dimension,
            multisampled,
            ..
        } => format!("Texture {name} dimension {dimension} multisampled {multisampled}"),
        other => format!("{other:?}"),
    }
}

fn compare_parameters(
    name: &str,
    keywords: &[String],
    mine: &rabex_shader::ProgramParameters,
    theirs: &rabex_shader::ProgramParameters,
) -> Result<()> {
    ensure!(
        mine.constant_buffers.len() == theirs.constant_buffers.len(),
        "{name} {keywords:?}: {} scopes, the original has {}",
        mine.constant_buffers.len(),
        theirs.constant_buffers.len()
    );
    for (mine, theirs) in mine.constant_buffers.iter().zip(&theirs.constant_buffers) {
        ensure!(
            mine.name == theirs.name && mine.size == theirs.size,
            "{name} {keywords:?}: scope {:?} ({}) vs the original's {:?} ({})",
            mine.name,
            mine.size,
            theirs.name,
            theirs.size
        );
        // a constant the original only carries in its common parameters doesn't
        // record its matrix column count; the cycle restores it, so the original's
        // unknown compares as a wildcard
        let constant = |c: &rabex_shader::Constant| -> (String, u32, Option<u32>, bool, u32, u32) {
            (
                c.name.clone(),
                c.data_type,
                c.columns,
                c.is_matrix,
                c.array_size,
                c.offset,
            )
        };
        let (mine_constants, their_constants): (Vec<_>, Vec<_>) = (
            mine.constants.iter().map(constant).collect(),
            theirs.constants.iter().map(constant).collect(),
        );
        let columns_match = |mine: &Option<u32>, theirs: &Option<u32>| match (mine, theirs) {
            (_, None) | (None, _) => true,
            (mine, theirs) => mine == theirs,
        };
        ensure!(
            mine_constants.len() == their_constants.len()
                && mine_constants
                    .iter()
                    .zip(&their_constants)
                    .all(|(mine, theirs)| {
                        mine.0 == theirs.0
                            && mine.1 == theirs.1
                            && columns_match(&mine.2, &theirs.2)
                            && mine.3 == theirs.3
                            && mine.4 == theirs.4
                            && mine.5 == theirs.5
                    }),
            "{name} {keywords:?}: scope {:?}: constants {:?} vs the original's {:?}",
            mine.name,
            mine.constants,
            theirs.constants
        );
        ensure!(
            format!("{:?}", mine.structs) == format!("{:?}", theirs.structs),
            "{name} {keywords:?}: scope {:?}: structs {:?} vs the original's {:?}",
            mine.name,
            mine.structs,
            theirs.structs
        );
    }
    let key = |b: &rabex_shader::Binding| format!("{b:?}");
    let (mut mine_bindings, mut their_bindings) = (
        mine.bindings.iter().map(key).collect::<Vec<_>>(),
        theirs.bindings.iter().map(key).collect::<Vec<_>>(),
    );
    mine_bindings.sort();
    their_bindings.sort();
    ensure!(
        mine_bindings == their_bindings,
        "{name} {keywords:?}: bindings {mine_bindings:?} vs the original's {their_bindings:?}"
    );
    Ok(())
}
