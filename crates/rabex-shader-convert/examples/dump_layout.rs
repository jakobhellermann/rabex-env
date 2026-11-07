//! Print how a fixture bundle's shaders lay out their sub programs per pass and stage.

use std::io::Cursor;
use std::path::Path;

use anyhow::{Context, Result};
use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig};
use rabex::objects::ClassId;
use rabex::typetree::NullTypeTreeProvider;
use rabex_shader::Shader;

fn main() -> Result<()> {
    let path = std::env::args().nth(1).context("missing bundle")?;
    let bundle = BundleFileReader::from_reader(
        Cursor::new(std::fs::read(Path::new(&path))?),
        &ExtractionConfig::default(),
    )?;
    let entry = bundle.main_serializedfile().context("no serialized file")?;
    let data = bundle.read_at_entry(entry)?;
    let file = SerializedFile::from_reader(&mut Cursor::new(data.as_slice()))?;
    for info in file.objects().filter(|o| o.m_ClassID == ClassId::Shader) {
        let shader: Shader = file.read(
            info,
            NullTypeTreeProvider,
            &mut Cursor::new(data.as_slice()),
        )?;
        println!(
            "{} platforms {:?} stageCounts {:?} keywords {:?}",
            shader.m_ParsedForm.m_Name,
            shader.platforms,
            shader.stageCounts,
            shader.m_ParsedForm.m_KeywordNames
        );
        for (p, pass) in shader
            .m_ParsedForm
            .m_SubShaders
            .iter()
            .flat_map(|s| &s.m_Passes)
            .enumerate()
        {
            println!(
                "  pass {p} mask {:#x} names {} platforms {:?} hashes {:?}",
                pass.m_ProgramMask,
                pass.m_NameIndices.len(),
                pass.m_Platforms,
                pass.m_EditorDataHash.as_ref().map(|h| h.len())
            );
            for (stage, program) in pass.programs() {
                let tiers = program.m_PlayerSubPrograms.as_deref().unwrap_or(&[]);
                if tiers.iter().all(|t| t.is_empty()) {
                    println!(
                        "    {stage:?} empty: tiers {} keywordmask {:?}",
                        tiers.len(),
                        program.m_SerializedKeywordStateMask
                    );
                    continue;
                }
                let (mut vectors, mut matrices, mut textures) = (0, 0, 0);
                let (mut buffers, mut constant_buffers, mut bindings) = (0, 0, 0);
                let (mut uavs, mut samplers) = (0, 0);
                if let Some(c) = &program.m_CommonParameters {
                    vectors = c.m_VectorParams.len();
                    matrices = c.m_MatrixParams.len();
                    textures = c.m_TextureParams.len();
                    buffers = c.m_BufferParams.len();
                    constant_buffers = c.m_ConstantBuffers.len();
                    bindings = c.m_ConstantBufferBindings.len();
                    uavs = c.m_UAVParams.len();
                    samplers = c.m_Samplers.len();
                }
                println!(
                    "    {stage:?} common v{vectors} m{matrices} t{textures} b{buffers} cb{constant_buffers} cbb{bindings} uav{uavs} s{samplers} keywordmask {:?} subprograms(legacy) {}",
                    program.m_SerializedKeywordStateMask,
                    program.m_SubPrograms.len()
                );
                let param_tiers = program.m_ParameterBlobIndices.as_deref().unwrap_or(&[]);
                for (tier, (subs, params)) in tiers.iter().zip(param_tiers).enumerate() {
                    for (sub, param) in subs.iter().zip(params) {
                        println!(
                            "      tier {tier} type {:2} blob {:3} params {:3} req {:#x} keywords {:?}",
                            sub.m_GpuProgramType,
                            sub.m_BlobIndex,
                            param,
                            sub.m_ShaderRequirements,
                            sub.m_KeywordIndices
                        );
                    }
                }
            }
        }
    }
    Ok(())
}
