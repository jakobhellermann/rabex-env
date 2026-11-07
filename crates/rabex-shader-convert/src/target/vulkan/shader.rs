//! Replacing a shader's D3D11 programs with Vulkan programs, laid out like Unity's Linux builds:
//! every pass variant is one program under `progVertex`, without common parameters.

use std::collections::BTreeSet;

use rayon::prelude::*;

use anyhow::{Context, Result, bail, ensure};
use rabex_shader::types::{SerializedPlayerSubProgram, SerializedProgramParameters};
use rabex_shader::variants::Stage;
use rabex_shader::{Platform, PlatformBlob, ProgramVariant, Shader};

use super::{VULKAN_PROGRAM_TYPE, from_d3d11, parameters, program};

/// The stage variants of one Vulkan program.
#[derive(Debug)]
pub struct PassVariant<'a, 'b> {
    pub tier: usize,
    /// Sorted by name.
    pub keywords: Vec<String>,
    pub stages: Vec<&'a ProgramVariant<'b>>,
}

/// The Vulkan variants of a pass: per tier, every combination of one variant per stage whose
/// keywords together select exactly that combination.
pub fn pass_variants<'a, 'b>(
    variants: &'a [ProgramVariant<'b>],
) -> Result<Vec<PassVariant<'a, 'b>>> {
    let keys: Vec<(Stage, usize, &[String])> = variants
        .iter()
        .map(|v| (v.stage, v.tier, v.keywords.as_slice()))
        .collect();
    Ok(combinations(&keys)
        .into_iter()
        .map(|combination| PassVariant {
            tier: combination.tier,
            keywords: combination.keywords,
            stages: combination.stages.iter().map(|&i| &variants[i]).collect(),
        })
        .collect())
}

/// One Vulkan variant of a pass, by the indices of its stage variants.
#[derive(Debug, PartialEq, Eq)]
pub struct Combination {
    pub tier: usize,
    /// Sorted by name.
    pub keywords: Vec<String>,
    /// One per stage, in translation order.
    pub stages: Vec<usize>,
}

/// The Vulkan variants of a pass's stage variants, each given as its stage, tier and keywords.
///
/// D3D11 stores each stage's programs with only the keywords that stage uses; a Vulkan
/// program holds every stage. For keywords `K` the D3D11 player takes per stage the variant
/// with the most keywords within `K`. A Vulkan variant is a keyword set that this choice
/// reproduces: per stage a variant that is the one strictly largest within `K`, their
/// keywords together `K`. Such a choice is fixed by `K`, so each set comes once.
///
/// The choices are searched stage by stage, in order of first appearance and the variants'
/// own order, so the variants come out in that order. A partial choice whose variants are
/// not the strictly largest within their union yet is dropped: a larger union only lets more
/// variants fit, so they could not become it.
pub fn combinations(variants: &[(Stage, usize, &[String])]) -> Vec<Combination> {
    let tiers: BTreeSet<usize> = variants.iter().map(|v| v.1).collect();
    let mut found = Vec::new();
    for tier in tiers {
        let in_tier: Vec<usize> = (0..variants.len())
            .filter(|&i| variants[i].1 == tier)
            .collect();
        // the tier's keywords as bits
        let mut names: Vec<&str> = in_tier
            .iter()
            .flat_map(|&i| variants[i].2.iter().map(String::as_str))
            .collect();
        names.sort_unstable();
        names.dedup();
        let mask = |keywords: &[String]| {
            let mut bits = Bits::new(names.len());
            for keyword in keywords {
                bits.set(
                    names
                        .binary_search(&keyword.as_str())
                        .expect("a name of the tier"),
                );
            }
            bits
        };
        let mut stages: Vec<StageVariants> = Vec::new();
        for &i in &in_tier {
            let (stage, _, keywords) = variants[i];
            let candidate = Candidate {
                index: i,
                keywords: mask(keywords),
                count: keywords.len(),
            };
            match stages.iter_mut().find(|s| s.stage == stage) {
                Some(found) => found.variants.push(candidate),
                None => stages.push(StageVariants {
                    stage,
                    variants: vec![candidate],
                }),
            }
        }
        // per stage, the variants by keyword count, largest first, for the fit checks
        let by_size: Vec<Vec<usize>> = stages
            .iter()
            .map(|s| {
                let mut order: Vec<usize> = (0..s.variants.len()).collect();
                order.sort_by_key(|&k| std::cmp::Reverse(s.variants[k].count));
                order
            })
            .collect();
        // whether variant `k` of stage `s` is the one strictly largest within `union`
        let largest_within = |s: usize, k: usize, union: &Bits| {
            let list = &stages[s].variants;
            let size = list[k].count;
            by_size[s]
                .iter()
                .take_while(|&&other| list[other].count >= size)
                .all(|&other| other == k || !list[other].keywords.within(union))
        };
        let mut chosen: Vec<usize> = Vec::with_capacity(stages.len());
        let mut unions: Vec<Bits> = vec![Bits::new(names.len())];
        search(
            &stages,
            &largest_within,
            &mut chosen,
            &mut unions,
            &mut |chosen, union| {
                let mut picked: Vec<(Stage, usize)> = chosen
                    .iter()
                    .enumerate()
                    .map(|(s, &k)| (stages[s].stage, stages[s].variants[k].index))
                    .collect();
                picked.sort_by_key(|&(stage, _)| {
                    super::TRANSLATION_ORDER.iter().position(|&o| o == stage)
                });
                found.push(Combination {
                    tier,
                    keywords: union.names(&names),
                    stages: picked.into_iter().map(|(_, i)| i).collect(),
                });
            },
        );
    }
    found
}

/// Choose a variant for the next stage after `chosen`, keeping only choices whose variants
/// are each the strictly largest within the union so far; `unions[d]` is the union of the
/// first `d` choices.
fn search(
    stages: &[StageVariants],
    largest_within: &impl Fn(usize, usize, &Bits) -> bool,
    chosen: &mut Vec<usize>,
    unions: &mut Vec<Bits>,
    found: &mut impl FnMut(&[usize], &Bits),
) {
    let depth = chosen.len();
    let Some(stage) = stages.get(depth) else {
        found(chosen, &unions[depth]);
        return;
    };
    for (k, candidate) in stage.variants.iter().enumerate() {
        let union = unions[depth].union(&candidate.keywords);
        chosen.push(k);
        if chosen
            .iter()
            .enumerate()
            .all(|(s, &k)| largest_within(s, k, &union))
        {
            unions.push(union);
            search(stages, largest_within, chosen, unions, found);
            unions.pop();
        }
        chosen.pop();
    }
}

/// One stage's variants in a tier, in their order.
struct StageVariants {
    stage: Stage,
    variants: Vec<Candidate>,
}

struct Candidate {
    /// Into the variants given.
    index: usize,
    keywords: Bits,
    /// Keywords named, which ranks the variants fitting a keyword set.
    count: usize,
}

/// A set of keywords of one tier, as bits.
#[derive(Clone, Debug)]
struct Bits(Vec<u64>);

impl Bits {
    fn new(count: usize) -> Self {
        Bits(vec![0; count.div_ceil(64)])
    }

    fn set(&mut self, bit: usize) {
        self.0[bit / 64] |= 1 << (bit % 64);
    }

    fn union(&self, other: &Bits) -> Bits {
        Bits(self.0.iter().zip(&other.0).map(|(a, b)| a | b).collect())
    }

    /// Whether every bit of `self` is in `other`.
    fn within(&self, other: &Bits) -> bool {
        self.0.iter().zip(&other.0).all(|(a, b)| a & !b == 0)
    }

    /// The names of the set bits, in the order of `names`.
    fn names(&self, names: &[&str]) -> Vec<String> {
        (0..names.len())
            .filter(|&bit| self.0[bit / 64] & (1 << (bit % 64)) != 0)
            .map(|bit| names[bit].to_owned())
            .collect()
    }
}

fn empty_parameters() -> SerializedProgramParameters {
    SerializedProgramParameters {
        m_VectorParams: Vec::new(),
        m_MatrixParams: Vec::new(),
        m_TextureParams: Vec::new(),
        m_BufferParams: Vec::new(),
        m_ConstantBuffers: Vec::new(),
        m_ConstantBufferBindings: Vec::new(),
        m_UAVParams: Vec::new(),
        m_Samplers: Vec::new(),
    }
}

/// Replace the D3D11 programs of a D3D11-only shader with Vulkan programs. `chunk_size` is the
/// segment size of the program data, see [`PlatformBlob::from_entries`].
/// Convert a copy of the shader, leaving the original untouched.
pub fn convert(shader: &Shader, chunk_size: usize) -> Result<Shader> {
    let mut shader = shader.clone();
    convert_inplace(&mut shader, chunk_size)?;
    Ok(shader)
}

/// Convert the shader in place: its D3D11 platform becomes a Vulkan platform.
/// [`translate_inplace`], then the check that every program announces what its modules
/// read and nothing they do not: a disagreement is a GPU page fault in the player.
pub fn convert_inplace(shader: &mut Shader, chunk_size: usize) -> Result<()> {
    translate_inplace(shader, chunk_size)?;
    crate::vulkan_bindings::verify(shader)?;
    Ok(())
}

/// One pass variant as Vulkan: its parameter blob and program, and where they go.
struct Converted {
    tier: usize,
    requirements: i64,
    keyword_indices: Vec<u16>,
    parameters: Vec<u8>,
    program: Vec<u8>,
}

fn translate_inplace(shader: &mut Shader, chunk_size: usize) -> Result<()> {
    let blobs = shader.platform_blobs()?;
    let [d3d11] = blobs.as_slice() else {
        anyhow::bail!("{} platforms, not D3D11 alone", blobs.len());
    };
    ensure!(
        d3d11.platform == Platform::D3D11,
        "platform {:?}, not D3D11",
        d3d11.platform
    );
    let Some(keyword_names) = &shader.m_ParsedForm.m_KeywordNames else {
        bail!("shader without m_KeywordNames; the Vulkan target builds Unity 2022+ shaders");
    };

    let mut entries: Vec<Vec<u8>> = Vec::new();
    // Per pass in order: Vulkan sub programs and parameter blob indices, by tier.
    let mut passes = Vec::new();
    for pass in shader
        .m_ParsedForm
        .m_SubShaders
        .iter()
        .flat_map(|s| &s.m_Passes)
    {
        let d3d11_variants = shader.pass_variants(pass, d3d11)?;
        let Some(player_tiers) = &pass.progVertex.m_PlayerSubPrograms else {
            bail!("{}: no player sub programs", pass.m_Name);
        };
        let tiers = player_tiers.len();
        let mut subs: Vec<Vec<SerializedPlayerSubProgram>> =
            (0..tiers).map(|_| Vec::new()).collect();
        let mut parameter_indices: Vec<Vec<u32>> = vec![Vec::new(); tiers];
        // the variants convert independently; only where their entries land follows the order
        let converted: Vec<Result<Converted>> = pass_variants(&d3d11_variants)?
            .par_iter()
            .map(|variant| {
                let context = || format!("{} {:?}", pass.m_Name, variant.keywords);
                let requirements = variant.stages[0].requirements;
                // TODO: combine the requirements of stages that disagree, once data with them turns up
                ensure!(
                    variant
                        .stages
                        .iter()
                        .all(|s| s.requirements == requirements),
                    "{}: stages with different requirements",
                    context()
                );
                let stages = from_d3d11(&variant.stages).with_context(context)?;
                let parameters = parameters(&stages, &variant.stages).with_context(context)?;
                let program = program(&stages, &variant.stages).with_context(context)?;
                let mut keyword_indices = variant
                    .keywords
                    .iter()
                    .map(|k| {
                        let index =
                            keyword_names.iter().position(|n| n == k).with_context(|| {
                                format!("{}: keyword {k} not in m_KeywordNames", context())
                            })?;
                        Ok(u16::try_from(index)?)
                    })
                    .collect::<Result<Vec<u16>>>()?;
                keyword_indices.sort();
                let tier = variant.tier;
                ensure!(
                    tier < tiers,
                    "{}: tier {tier} beyond progVertex's {tiers}",
                    context()
                );
                Ok(Converted {
                    tier,
                    requirements,
                    keyword_indices,
                    parameters: parameters.to_blob()?,
                    program: program.to_bytes()?,
                })
            })
            .collect();
        for converted in converted {
            let Converted {
                tier,
                requirements,
                keyword_indices,
                parameters,
                program,
            } = converted?;
            parameter_indices[tier].push(u32::try_from(entries.len())?);
            entries.push(parameters);
            subs[tier].push(SerializedPlayerSubProgram {
                m_BlobIndex: u32::try_from(entries.len())?,
                m_KeywordIndices: keyword_indices,
                m_ShaderRequirements: requirements,
                m_GpuProgramType: i8::try_from(VULKAN_PROGRAM_TYPE)?,
            });
            entries.push(program);
        }
        passes.push((subs, parameter_indices));
    }

    let passes_mut = shader
        .m_ParsedForm
        .m_SubShaders
        .iter_mut()
        .flat_map(|s| &mut s.m_Passes);
    for (pass, (subs, parameter_indices)) in passes_mut.zip(passes) {
        for program in [
            &mut pass.progVertex,
            &mut pass.progFragment,
            &mut pass.progGeometry,
            &mut pass.progHull,
            &mut pass.progDomain,
            &mut pass.progRayTracing,
        ] {
            ensure!(
                program.m_SubPrograms.is_empty(),
                "{}: legacy m_SubPrograms",
                pass.m_Name
            );
            program.m_PlayerSubPrograms = Some(Vec::new());
            program.m_ParameterBlobIndices = Some(Vec::new());
            program.m_CommonParameters = Some(empty_parameters());
        }
        pass.progVertex.m_PlayerSubPrograms = Some(subs);
        pass.progVertex.m_ParameterBlobIndices = Some(parameter_indices);
        pass.m_NameIndices.clear();
        if let Some(platforms) = &mut pass.m_Platforms {
            *platforms = vec![u8::try_from(Platform::Vulkan.id())?];
        }
        // TODO: what the editor data hash is a hash of; Unity's Linux builds keep one per platform,
        // some bundles none
        if let Some(hashes) = &pass.m_EditorDataHash {
            ensure!(hashes.len() <= 1, "{} editor data hashes", hashes.len());
        }
    }
    shader.set_platform_blobs(&[PlatformBlob::from_entries(
        Platform::Vulkan,
        &entries,
        chunk_size,
    )?])
}
