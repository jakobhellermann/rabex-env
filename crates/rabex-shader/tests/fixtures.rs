//! Compare program variants and their parameters against what Unity's `CompileVariant` reports.
//! The fixtures and their generator live in a separate Unity project (`ShaderFixtures.cs`).

use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use std::str::FromStr;

use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig};
use rabex::objects::ClassId;
use rabex::typetree::NullTypeTreeProvider;
use rabex_shader::{
    Binding, Constant, EntryKind, Platform, PlatformBlob, ProgramParameters, Shader,
    StructConstant, SubProgram, unity,
};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    unity_version: String,
    bundles: Vec<TargetBundle>,
    shaders: Vec<ShaderEntry>,
    enums: Vec<EnumEntry>,
}

#[derive(Debug, Deserialize)]
struct EnumEntry {
    name: String,
    values: Vec<EnumValue>,
}

#[derive(Debug, Deserialize)]
struct EnumValue {
    name: String,
    value: i32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct TargetBundle {
    target: String,
    bundle: String,
    platforms: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ShaderEntry {
    name: String,
    variants: Vec<VariantEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct VariantEntry {
    sub_shader: usize,
    pass: usize,
    pass_name: String,
    stage: String,
    platform: String,
    keywords: Vec<String>,
    code: String,
    attributes: Vec<String>,
    constant_buffers: Vec<ConstantBufferEntry>,
    textures: Vec<TextureEntry>,
}

#[derive(Debug, Deserialize)]
struct ConstantBufferEntry {
    name: String,
    size: u32,
    fields: Vec<ConstantEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConstantEntry {
    name: String,
    index: u32,
    constant_type: String,
    data_type: String,
    rows: u32,
    columns: u32,
    array_size: u32,
    struct_size: u32,
    struct_fields: Vec<StructFieldEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StructFieldEntry {
    name: String,
    index: u32,
    constant_type: String,
    data_type: String,
    rows: u32,
    columns: u32,
    array_size: u32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct TextureEntry {
    name: String,
    index: i32,
    sampler_index: i32,
    dim: String,
    multisampled: bool,
    array_size: u32,
}

/// Identifies a variant across the bundle and the manifest.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct VariantKey {
    shader: String,
    sub_shader: usize,
    pass: usize,
    stage: String,
    platform: String,
    keywords: Vec<String>,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Field {
    Plain {
        name: String,
        offset: u32,
        matrix: bool,
        data_type: String,
        rows: u32,
        columns: Option<u32>,
        array_size: u32,
    },
    Struct {
        name: String,
        offset: u32,
        array_size: u32,
        struct_size: u32,
        members: Vec<Field>,
    },
}

/// Constant buffer name to size and fields, sorted by name.
type Layout = BTreeMap<String, (u32, Vec<Field>)>;
/// Texture name to index, sampler index, dimension and multisampling.
type Textures = BTreeMap<String, (i32, i32, String, bool)>;

struct Variant {
    code: Vec<u8>,
    parameters: ProgramParameters,
}

fn unity_name(all: &[(&'static str, u32)], value: u32) -> Result<String> {
    all.iter()
        .find(|(_, v)| *v == value)
        .map(|(name, _)| (*name).to_owned())
        .with_context(|| format!("no Unity name for {value}"))
}

fn data_type_name(data_type: u32) -> Result<String> {
    unity_name(unity::shader_param_type::ALL, data_type)
}

fn dimension_name(dimension: u32) -> Result<String> {
    unity_name(unity::texture_dimension::ALL, dimension)
}

fn platform_name(platform: Platform) -> Result<String> {
    unity_name(unity::shader_compiler_platform::ALL, platform.id())
}

fn plain(c: &Constant) -> Result<Field> {
    Ok(Field::Plain {
        name: c.name.clone(),
        offset: c.offset,
        matrix: c.is_matrix,
        data_type: data_type_name(c.data_type)?,
        rows: c.rows,
        columns: c.columns,
        array_size: c.array_size,
    })
}

fn structure(s: &StructConstant) -> Result<Field> {
    let mut members = s.members.iter().map(plain).collect::<Result<Vec<_>>>()?;
    members.sort();
    Ok(Field::Struct {
        name: s.name.clone(),
        offset: s.offset,
        array_size: s.array_size,
        struct_size: s.struct_size,
        members,
    })
}

/// Without the unnamed scope, whose constants (like Vulkan specialization constants) Unity doesn't report.
fn layout(parameters: &ProgramParameters) -> Result<Layout> {
    let (_, named) = parameters
        .constant_buffers
        .split_first()
        .context("no unnamed scope")?;
    named
        .iter()
        .map(|cb| {
            let mut fields = cb.constants.iter().map(plain).collect::<Result<Vec<_>>>()?;
            fields.extend(
                cb.structs
                    .iter()
                    .map(structure)
                    .collect::<Result<Vec<_>>>()?,
            );
            fields.sort();
            Ok((cb.name.clone(), (cb.size, fields)))
        })
        .collect()
}

fn expected_plain(
    name: &str,
    offset: u32,
    constant_type: &str,
    data_type: &str,
    rows: u32,
    columns: u32,
    array_size: u32,
) -> Result<Field> {
    let matrix = match constant_type {
        "Vector" => false,
        "Matrix" => true,
        other => bail!("{name}: unknown constant type {other}"),
    };
    Ok(Field::Plain {
        name: name.to_owned(),
        offset,
        matrix,
        data_type: data_type.to_owned(),
        rows,
        columns: Some(columns),
        array_size,
    })
}

fn expected_member(c: &StructFieldEntry) -> Result<Field> {
    expected_plain(
        &c.name,
        c.index,
        &c.constant_type,
        &c.data_type,
        c.rows,
        c.columns,
        c.array_size,
    )
}

fn expected_field(c: &ConstantEntry) -> Result<Field> {
    Ok(match c.constant_type.as_str() {
        "Struct" => {
            let mut members = c
                .struct_fields
                .iter()
                .map(expected_member)
                .collect::<Result<Vec<_>>>()?;
            members.sort();
            Field::Struct {
                name: c.name.clone(),
                offset: c.index,
                array_size: c.array_size,
                struct_size: c.struct_size,
                members,
            }
        }
        constant_type => expected_plain(
            &c.name,
            c.index,
            constant_type,
            &c.data_type,
            c.rows,
            c.columns,
            c.array_size,
        )?,
    })
}

/// Matrices from `m_CommonParameters` don't know their column count, so it isn't compared.
fn forget_unknown_columns(actual: &[Field], expected: &mut [Field]) {
    for (a, e) in actual.iter().zip(expected) {
        match (a, e) {
            (Field::Plain { columns: None, .. }, Field::Plain { columns, .. }) => *columns = None,
            (Field::Struct { members: a, .. }, Field::Struct { members: e, .. }) => {
                forget_unknown_columns(a, e)
            }
            _ => {}
        }
    }
}

fn expected_layout(entry: &VariantEntry, actual: &Layout) -> Result<Layout> {
    entry
        .constant_buffers
        .iter()
        .map(|cb| {
            let mut fields = cb
                .fields
                .iter()
                .map(expected_field)
                .collect::<Result<Vec<_>>>()?;
            fields.sort();
            if let Some((_, actual_fields)) = actual.get(&cb.name) {
                forget_unknown_columns(actual_fields, &mut fields);
            }
            Ok((cb.name.clone(), (cb.size, fields)))
        })
        .collect()
}

fn textures(parameters: &ProgramParameters) -> Result<Textures> {
    parameters
        .bindings
        .iter()
        .filter_map(|binding| match binding {
            Binding::Texture {
                name,
                index,
                sampler_index,
                dimension,
                multisampled,
            } => Some(
                dimension_name(*dimension)
                    .map(|dim| (name.clone(), (*index, *sampler_index, dim, *multisampled))),
            ),
            _ => None,
        })
        .collect()
}

fn expected_textures(entry: &VariantEntry) -> Textures {
    entry
        .textures
        .iter()
        .map(|t| {
            let value = (t.index, t.sampler_index, t.dim.clone(), t.multisampled);
            (t.name.clone(), value)
        })
        .collect()
}

/// The shaders of a version's bundles.
fn bundle_shaders(dir: &Path, manifest: &Manifest) -> Result<Vec<Shader>> {
    let mut shaders = Vec::new();
    for target in &manifest.bundles {
        let path = dir.join(&target.bundle);
        let bundle = BundleFileReader::from_reader(
            Cursor::new(std::fs::read(&path).with_context(|| format!("{}", path.display()))?),
            &ExtractionConfig::default(),
        )?;
        let entry = bundle
            .main_serializedfile()
            .context("bundle without serialized file")?;
        let data = bundle.read_at_entry(entry)?;
        let file = SerializedFile::from_reader(&mut Cursor::new(data.as_slice()))?;
        for info in file.objects().filter(|o| o.m_ClassID == ClassId::Shader) {
            shaders.push(file.read(
                info,
                NullTypeTreeProvider,
                &mut Cursor::new(data.as_slice()),
            )?);
        }
    }
    Ok(shaders)
}

/// Every program variant in a version's bundles, keyed like the manifest.
fn bundle_variants(dir: &Path, manifest: &Manifest) -> Result<BTreeMap<VariantKey, Variant>> {
    let mut variants = BTreeMap::new();
    for shader in bundle_shaders(dir, manifest)? {
        let name = shader.m_ParsedForm.m_Name.clone();
        for blob in shader.platform_blobs()? {
            let platform = platform_name(blob.platform)?;
            for (s, sub_shader) in shader.m_ParsedForm.m_SubShaders.iter().enumerate() {
                for (p, pass) in sub_shader.m_Passes.iter().enumerate() {
                    if pass.progVertex.m_PlayerSubPrograms.is_none() {
                        // Unity 2020 stores keyword masks, and the names variant enumeration
                        // needs are not in the shader
                        continue;
                    }
                    let pass_variants = shader
                        .pass_variants(pass, &blob)
                        .with_context(|| format!("{name} {platform} pass {p}"))?;
                    for variant in pass_variants {
                        let mut keywords = variant.keywords;
                        keywords.sort();
                        let key = VariantKey {
                            shader: name.clone(),
                            sub_shader: s,
                            pass: p,
                            stage: format!("{:?}", variant.stage),
                            platform: platform.to_owned(),
                            keywords,
                        };
                        let value = Variant {
                            code: variant.program.code.to_vec(),
                            parameters: variant.parameters,
                        };
                        ensure!(
                            variants.insert(key.clone(), value).is_none(),
                            "{key:?} is in the bundles twice"
                        );
                    }
                }
            }
        }
    }
    Ok(variants)
}

/// Each Unity version's manifest with the variants of its bundles.
/// One version's manifest, fixture directory and parsed variants.
type Version = (Manifest, PathBuf, BTreeMap<VariantKey, Variant>);

fn fixtures() -> Result<Vec<Version>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut dirs: Vec<_> = std::fs::read_dir(&root)?
        .map(|e| Ok(e?.path()))
        .collect::<Result<_>>()?;
    dirs.sort();
    ensure!(!dirs.is_empty(), "no fixtures in {}", root.display());
    dirs.into_iter()
        .map(|dir| {
            let manifest: Manifest =
                serde_json::from_slice(&std::fs::read(dir.join("shaders.json"))?)?;
            let variants =
                bundle_variants(&dir, &manifest).with_context(|| manifest.unity_version.clone())?;
            Ok((manifest, dir, variants))
        })
        .collect()
}

fn key(shader: &ShaderEntry, entry: &VariantEntry) -> VariantKey {
    let mut keywords = entry.keywords.clone();
    keywords.sort();
    VariantKey {
        shader: shader.name.clone(),
        sub_shader: entry.sub_shader,
        pass: entry.pass,
        stage: entry.stage.clone(),
        platform: entry.platform.clone(),
        keywords,
    }
}

/// The bundle variant Unity built for a manifest entry: the same keywords, or for keywords another
/// stage uses, the stage's variant with the most keywords among them.
fn resolve<'a>(
    variants: &'a BTreeMap<VariantKey, Variant>,
    key: &VariantKey,
) -> Option<(&'a VariantKey, &'a Variant)> {
    if let Some(found) = variants.get_key_value(key) {
        return Some(found);
    }
    let candidates = variants.iter().filter(|(k, _)| {
        (&k.shader, k.sub_shader, k.pass, &k.stage, &k.platform)
            == (
                &key.shader,
                key.sub_shader,
                key.pass,
                &key.stage,
                &key.platform,
            )
            && k.keywords.iter().all(|w| key.keywords.contains(w))
    });
    candidates.max_by_key(|(k, _)| k.keywords.len())
}

#[test]
fn programs_match_unity() -> Result<()> {
    let mut failures = Vec::new();
    for (manifest, dir, variants) in fixtures()? {
        let version = &manifest.unity_version;
        let mut used_keys = BTreeSet::new();
        for shader in &manifest.shaders {
            for entry in &shader.variants {
                let key = key(shader, entry);
                let code = std::fs::read(dir.join(&entry.code))?;
                let resolved = resolve(&variants, &key);
                if let Some((resolved_key, _)) = resolved {
                    used_keys.insert(resolved_key.clone());
                }
                match (resolved.map(|(_, v)| v), code.is_empty()) {
                    (Some(variant), false) if variant.code == code => {}
                    (Some(variant), false) => failures.push(format!(
                        "{version} {key:?}: {} code bytes, Unity compiled {}",
                        variant.code.len(),
                        code.len()
                    )),
                    (None, false) => {
                        failures.push(format!("{version} {key:?}: missing from the bundles"))
                    }
                    (Some(_), true) => {
                        failures.push(format!("{version} {key:?}: Unity compiled nothing"))
                    }
                    (None, true) => {}
                }
            }
        }
        for key in variants.keys().filter(|key| !used_keys.contains(key)) {
            failures.push(format!("{version} {key:?}: not in the manifest"));
        }
    }
    ensure!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
    Ok(())
}

#[test]
fn parameters_match_unity() -> Result<()> {
    let mut failures = Vec::new();
    let mut compared = 0;
    for (manifest, _, variants) in fixtures()? {
        let version = &manifest.unity_version;
        for shader in &manifest.shaders {
            for entry in &shader.variants {
                let key = key(shader, entry);
                let Some((_, variant)) = resolve(&variants, &key) else {
                    continue;
                };
                compared += 1;
                let actual_layout = layout(&variant.parameters)?;
                let expected_layout = expected_layout(entry, &actual_layout)?;
                if actual_layout != expected_layout {
                    failures.push(format!(
                        "{version} {key:?} constant buffers:\n  got      {actual_layout:?}\n  expected {expected_layout:?}"
                    ));
                }
                let actual_textures = textures(&variant.parameters)?;
                let expected_textures = expected_textures(entry);
                if actual_textures != expected_textures {
                    failures.push(format!(
                        "{version} {key:?} textures:\n  got      {actual_textures:?}\n  expected {expected_textures:?}"
                    ));
                }
            }
        }
    }
    ensure!(compared > 0, "no variants compared");
    ensure!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
    Ok(())
}

/// Unity's default `m_DefaultShaderChunkSizeInMB`, which the fixture projects use.
const FIXTURE_CHUNK_SIZE: usize = 16 * 1024 * 1024;

#[test]
fn blobs_roundtrip() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut checked = 0;
    for dir in std::fs::read_dir(&root)? {
        let dir = dir?.path();
        let manifest: Manifest = serde_json::from_slice(&std::fs::read(dir.join("shaders.json"))?)?;
        for mut shader in bundle_shaders(&dir, &manifest)? {
            let name = format!("{} {}", manifest.unity_version, shader.m_ParsedForm.m_Name);
            let blobs = shader.platform_blobs()?;
            for (i, blob) in blobs.iter().enumerate() {
                let entries = blob.entry_data()?;
                let kinds = shader.entry_kinds(blob.platform, entries.len())?;
                for (index, (data, kind)) in entries.iter().zip(&kinds).enumerate() {
                    let rewritten = match kind {
                        Some(EntryKind::Program) => SubProgram::parse(data)?.to_bytes()?,
                        Some(EntryKind::Parameters) => ProgramParameters::parse(data)?.to_blob()?,
                        None => bail!("{name} {:?}: entry {index} is unreferenced", blob.platform),
                    };
                    ensure!(
                        rewritten == *data,
                        "{name} {:?}: entry {index} ({kind:?}) differs after rewriting",
                        blob.platform
                    );
                    checked += 1;
                }
                let rebuilt =
                    PlatformBlob::from_entries(blob.platform, &entries, FIXTURE_CHUNK_SIZE)?;
                ensure!(
                    rebuilt.segments == blob.segments,
                    "{name} {:?}: segments differ after rebuilding",
                    blob.platform
                );
                if let Some(stage_counts) = shader.stageCounts.as_ref() {
                    assert_eq!(
                        shader.stage_count(blob.platform)?,
                        stage_counts[i],
                        "{name}"
                    );
                }
            }
            let platforms = shader.platforms.clone();
            let stage_counts = shader.stageCounts.clone();
            shader.set_platform_blobs(&blobs)?;
            assert_eq!(shader.platforms, platforms, "{name}");
            assert_eq!(shader.stageCounts, stage_counts, "{name}");
            let reread: Vec<_> = shader
                .platform_blobs()?
                .into_iter()
                .map(|b| b.segments)
                .collect();
            let original: Vec<_> = blobs.into_iter().map(|b| b.segments).collect();
            ensure!(
                reread == original,
                "{name}: blobs differ after recompressing"
            );
        }
    }
    ensure!(checked > 0, "no entries checked");
    Ok(())
}

#[test]
fn vulkan_programs_roundtrip() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut checked = 0;
    for dir in std::fs::read_dir(&root)? {
        let dir = dir?.path();
        let manifest: Manifest = serde_json::from_slice(&std::fs::read(dir.join("shaders.json"))?)?;
        for shader in bundle_shaders(&dir, &manifest)? {
            for blob in shader
                .platform_blobs()?
                .iter()
                .filter(|b| b.platform == Platform::Vulkan)
            {
                for pass in shader
                    .m_ParsedForm
                    .m_SubShaders
                    .iter()
                    .flat_map(|s| &s.m_Passes)
                {
                    if pass.progVertex.m_PlayerSubPrograms.is_none() {
                        // Unity 2020 stores keyword masks, and the names variant enumeration
                        // needs are not in the shader
                        continue;
                    }
                    for variant in shader.pass_variants(pass, blob)? {
                        let program =
                            rabex_shader::vulkan::VulkanProgram::parse(variant.program.code)?;
                        let separate_samplers =
                            variant.parameters.bindings.iter().any(|b| match b {
                                Binding::Texture { sampler_index, .. } => *sampler_index >= 0,
                                Binding::Sampler { .. } => true,
                                _ => false,
                            });
                        let combined_only = program.flags
                            & rabex_shader::vulkan::flags::COMBINED_SAMPLERS_ONLY
                            != 0;
                        ensure!(
                            combined_only != separate_samplers,
                            "{} {}: flags {:#x} but separate samplers {separate_samplers}",
                            shader.m_ParsedForm.m_Name,
                            pass.m_Name,
                            program.flags
                        );
                        ensure!(
                            program.to_bytes()? == variant.program.code,
                            "{} {}: Vulkan program differs after rewriting",
                            shader.m_ParsedForm.m_Name,
                            pass.m_Name
                        );
                        checked += 1;
                    }
                }
            }
        }
    }
    // 2 versions with Vulkan programs in expected form; the instancing shader's two
    // keyword sets add two per version
    ensure!(
        checked == 34,
        "checked {checked} Vulkan programs, the fixtures have 17 per Unity version"
    );
    Ok(())
}

/// Our copies of Unity's enums hold exactly what the fixtures' editors define, across versions.
#[test]
fn unity_enums_match_editor() -> Result<()> {
    let ours = [
        (
            unity::shader_compiler_platform::UNITY_NAME,
            unity::shader_compiler_platform::ALL,
        ),
        (
            unity::texture_dimension::UNITY_NAME,
            unity::texture_dimension::ALL,
        ),
        (
            unity::vertex_attribute::UNITY_NAME,
            unity::vertex_attribute::ALL,
        ),
        (
            unity::shader_param_type::UNITY_NAME,
            unity::shader_param_type::ALL,
        ),
        (
            unity::graphics_device_type::UNITY_NAME,
            unity::graphics_device_type::ALL,
        ),
    ];
    let mut seen: BTreeMap<&str, BTreeSet<(String, u32)>> = BTreeMap::new();
    for (manifest, _, _) in fixtures()? {
        let dumped: BTreeSet<&str> = manifest.enums.iter().map(|e| e.name.as_str()).collect();
        // ShaderParamType is not dumped below Unity 2022, where its guard keeps it from the
        // fixture scripts
        let expected: BTreeSet<&str> = ours
            .iter()
            .map(|(name, _)| *name)
            .filter(|name| *name != unity::shader_param_type::UNITY_NAME || dumped.contains(name))
            .collect();
        assert_eq!(dumped, expected, "{}", manifest.unity_version);
        for entry in &manifest.enums {
            let (unity_name, all) = ours.iter().find(|(name, _)| *name == entry.name).unwrap();
            let all: BTreeSet<(String, u32)> =
                all.iter().map(|(n, v)| ((*n).to_owned(), *v)).collect();
            // Our tables carry the values Unity 2022.3 has. Older versions shifted values
            // (GameCore is 21 in 2020.2, 25 in 2022.3), so their entries are compared by name.
            let strict = rabex::UnityVersion::from_str(&manifest.unity_version).unwrap()
                >= rabex::UnityVersion::new(2022, 3, 0);
            for value in &entry.values {
                // C# ints, serialized as their bit pattern
                let item = (value.name.clone(), value.value as u32);
                if strict {
                    assert!(
                        all.contains(&item),
                        "{} {unity_name}: {item:?} missing",
                        manifest.unity_version
                    );
                    seen.entry(unity_name).or_default().insert(item);
                } else {
                    assert!(
                        all.iter().any(|(name, _)| *name == item.0),
                        "{} {unity_name}: {} missing",
                        manifest.unity_version,
                        value.name
                    );
                }
            }
        }
    }
    for (unity_name, all) in ours {
        let all: BTreeSet<(String, u32)> = all.iter().map(|(n, v)| ((*n).to_owned(), *v)).collect();
        assert_eq!(seen[unity_name], all, "{unity_name}");
    }
    for &(name, id) in unity::shader_compiler_platform::ALL {
        if name != "None" {
            let platform = Platform::from_id(id);
            assert!(!matches!(platform, Platform::Unknown(_)), "{name}");
            assert_eq!(platform.id(), id, "{name}");
        }
    }
    Ok(())
}
