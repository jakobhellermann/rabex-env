//! What a shader's program blobs mean: the compiled [`ProgramVariant`]s of every pass,
//! which blob entry holds what ([`EntryKind`]) and how many stages a platform carries.
//!
//! This interprets the raw data of the `types` and blob layers; shaders whose sub
//! programs carry keyword masks instead of names (Unity 2020) fail loudly here.

use anyhow::{Context, Result, bail, ensure};

use crate::types::{SerializedPass, SerializedProgram};
use crate::{PlatformBlob, ProgramParameters, Shader, SubProgramView};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Vertex,
    Fragment,
    Geometry,
    Hull,
    Domain,
    RayTracing,
    /// A compute shader's kernel, which no pass has.
    Compute,
}

impl SerializedPass {
    pub fn programs(&self) -> [(Stage, &SerializedProgram); 6] {
        [
            (Stage::Vertex, &self.progVertex),
            (Stage::Fragment, &self.progFragment),
            (Stage::Geometry, &self.progGeometry),
            (Stage::Hull, &self.progHull),
            (Stage::Domain, &self.progDomain),
            (Stage::RayTracing, &self.progRayTracing),
        ]
    }

    pub fn programs_mut(&mut self) -> [(Stage, &mut SerializedProgram); 6] {
        [
            (Stage::Vertex, &mut self.progVertex),
            (Stage::Fragment, &mut self.progFragment),
            (Stage::Geometry, &mut self.progGeometry),
            (Stage::Hull, &mut self.progHull),
            (Stage::Domain, &mut self.progDomain),
            (Stage::RayTracing, &mut self.progRayTracing),
        ]
    }
}

/// One compiled variant of a pass's program.
///
/// OpenGLCore and Vulkan programs contain every stage of the pass and are listed as [`Stage::Vertex`].
#[derive(Debug)]
/// One compiled variant of a pass, borrowed from the platform blob it lives in.
pub struct ProgramVariant<'a> {
    pub stage: Stage,
    /// The outer index of `m_PlayerSubPrograms`.
    pub tier: usize,
    pub keywords: Vec<String>,
    /// `m_ShaderRequirements`
    pub requirements: i64,
    pub program: SubProgramView<'a>,
    /// `m_CommonParameters` merged with the variant's parameter blob.
    pub parameters: ProgramParameters,
}

impl Shader {
    /// Every variant of `pass` compiled for the platform of `blob`.
    pub fn pass_variants<'a>(
        &self,
        pass: &SerializedPass,
        blob: &'a PlatformBlob,
    ) -> Result<Vec<ProgramVariant<'a>>> {
        let entries = blob.entries()?;
        let entry = |index: u32| -> Result<&[u8]> {
            let location = *entries
                .get(index as usize)
                .with_context(|| format!("blob index {index} out of range"))?;
            blob.entry(location)
        };
        let keyword_names = self.m_ParsedForm.m_KeywordNames.as_deref().unwrap_or(&[]);

        let mut variants = Vec::new();
        for (stage, program) in pass.programs() {
            let Some(player_tiers) = &program.m_PlayerSubPrograms else {
                bail!(
                    "{stage:?}: no player sub programs; Unity 2020 sub programs carry \
                     keyword masks instead of the names variant enumeration needs"
                );
            };
            let parameter_indices = program.m_ParameterBlobIndices.as_ref().unwrap();
            ensure!(
                player_tiers.len() == parameter_indices.len(),
                "{stage:?}: sub program and parameter blob tiers disagree"
            );
            let common = ProgramParameters::from_common(
                program
                    .m_CommonParameters
                    .as_ref()
                    .expect("player sub programs imply common parameters"),
                &pass.m_NameIndices,
            )
            .with_context(|| format!("{stage:?}: common parameters"))?;
            let tiers = player_tiers.iter().zip(parameter_indices);
            for (tier, (subs, parameter_indices)) in tiers.enumerate() {
                ensure!(
                    subs.len() == parameter_indices.len(),
                    "{stage:?} tier {tier}: sub programs and parameter blobs disagree in count"
                );
                for (sub, &parameter_index) in subs.iter().zip(parameter_indices) {
                    let program_type = u32::try_from(sub.m_GpuProgramType)?;
                    if !blob.platform.runs(program_type)? {
                        continue;
                    }
                    let context = || format!("{stage:?} tier {tier} blob {}", sub.m_BlobIndex);
                    let parsed =
                        SubProgramView::parse(entry(sub.m_BlobIndex)?).with_context(context)?;
                    ensure!(
                        parsed.program_type == program_type,
                        "{}: program type {} disagrees with m_GpuProgramType {program_type}",
                        context(),
                        parsed.program_type
                    );
                    let own = ProgramParameters::parse(entry(parameter_index)?)
                        .with_context(|| format!("{}: parameters {parameter_index}", context()))?;
                    let keywords = sub
                        .m_KeywordIndices
                        .iter()
                        .map(|&i| {
                            keyword_names
                                .get(i as usize)
                                .cloned()
                                .with_context(|| format!("{}: keyword index {i}", context()))
                        })
                        .collect::<Result<_>>()?;
                    variants.push(ProgramVariant {
                        stage,
                        tier,
                        keywords,
                        requirements: sub.m_ShaderRequirements,
                        program: parsed,
                        parameters: ProgramParameters::merge(common.clone(), own)
                            .with_context(context)?,
                    });
                }
            }
        }
        Ok(variants)
    }
}

/// What an entry of a platform's blob holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    Program,
    Parameters,
}

impl Shader {
    /// The kind of every entry of `platform`'s blob, by index; `None` for entries no variant references.
    pub fn entry_kinds(
        &self,
        platform: crate::Platform,
        count: usize,
    ) -> Result<Vec<Option<EntryKind>>> {
        let mut kinds = vec![None; count];
        let mut mark = |index: u32, kind: EntryKind| -> Result<()> {
            let slot = kinds
                .get_mut(index as usize)
                .with_context(|| format!("entry {index} out of range of {count}"))?;
            ensure!(
                slot.is_none_or(|existing| existing == kind),
                "entry {index} is both {:?} and {kind:?}",
                slot.unwrap()
            );
            *slot = Some(kind);
            Ok(())
        };
        for pass in self
            .m_ParsedForm
            .m_SubShaders
            .iter()
            .flat_map(|s| &s.m_Passes)
        {
            for (_, program) in pass.programs() {
                let Some(player_tiers) = &program.m_PlayerSubPrograms else {
                    // Unity 2020's legacy sub programs reference the blob directly
                    for sub in &program.m_SubPrograms {
                        if platform.runs(u32::try_from(sub.m_GpuProgramType)?)? {
                            mark(sub.m_BlobIndex, EntryKind::Program)?;
                        }
                    }
                    continue;
                };
                let subs = player_tiers.iter().flatten();
                let parameters = program.m_ParameterBlobIndices.iter().flatten().flatten();
                for (sub, &parameters) in subs.zip(parameters) {
                    if platform.runs(u32::try_from(sub.m_GpuProgramType)?)? {
                        mark(sub.m_BlobIndex, EntryKind::Program)?;
                        mark(parameters, EntryKind::Parameters)?;
                    }
                }
            }
        }
        Ok(kinds)
    }
}
