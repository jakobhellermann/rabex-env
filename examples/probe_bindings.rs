//! Compare a pass's bindings across the fixture versions: 2020.2 inline tables against
//! 2022+ parameter blobs, and the entries' keyword lists side by side.

use std::io::Cursor;
use std::path::Path;

use anyhow::{Context, Result};
use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig};
use rabex::objects::ClassId;
use rabex::typetree::NullTypeTreeProvider;
use rabex_shader::{Binding, ProgramParameters, Shader, SubProgramView, SubProgramViewLayout};

fn main() -> Result<()> {
    let root = Path::new(
        &std::env::args()
            .nth(1)
            .context("fixture version directory")?,
    )
    .join("shaders");
    let want = std::env::args().nth(2);
    let bundle = BundleFileReader::from_reader(
        Cursor::new(std::fs::read(&root).with_context(|| root.display().to_string())?),
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
        if want.is_none() || want.as_deref() == Some(shader.m_ParsedForm.m_Name.as_str()) {
            inspect(&shader)?;
        }
    }
    Ok(())
}

fn short(binding: &Binding) -> String {
    match binding {
        Binding::Texture {
            name,
            index,
            sampler_index,
            ..
        } => format!("tex {name}@{index} s{sampler_index}"),
        Binding::ConstantBuffer { name, index, .. } => format!("cb {name}@{index}"),
        Binding::Buffer { name, index, .. } => format!("buf {name}@{index}"),
        Binding::Uav { name, index, .. } => format!("uav {name}@{index}"),
        Binding::Sampler { name, index, .. } => format!("smp {name}@{index}"),
    }
}

fn inspect(shader: &Shader) -> Result<()> {
    let name = &shader.m_ParsedForm.m_Name;
    for (si, sub) in shader.m_ParsedForm.m_SubShaders.iter().enumerate() {
        for (pi, pass) in sub.m_Passes.iter().enumerate() {
            for (stage, program) in pass.programs() {
                let Some(common) = &program.m_CommonParameters else {
                    continue;
                };
                println!("{name} {si}/{pi} {stage:?} common:");
                for cb in &common.m_ConstantBuffers {
                    println!("  scope index {} size {}", cb.m_NameIndex, cb.m_Size);
                    for v in &cb.m_VectorParams {
                        println!(
                            "    vec idx {} {}x{} @{}",
                            v.m_NameIndex, v.m_Type, v.m_Dim, v.m_Index
                        );
                    }
                    for m in &cb.m_MatrixParams {
                        println!(
                            "    mat idx {} rows {} @{}",
                            m.m_NameIndex, m.m_RowCount, m.m_Index
                        );
                    }
                }
            }
        }
    }
    for blob in shader.platform_blobs()? {
        println!("{name} {:?}:", blob.platform);
        for (i, loc) in blob.entries()?.iter().cloned().enumerate() {
            let bytes = blob.entry(loc)?;
            if let Ok(view) = SubProgramView::parse(bytes) {
                let (locals, bindings) = match &view.layout {
                    SubProgramViewLayout::V201806140 {
                        local_keywords,
                        parameters,
                    } => (format!("{local_keywords:?}"), parameters.bindings.clone()),
                    _ => ("-".to_string(), Vec::new()),
                };
                println!(
                    "  entry {i} type {} keywords {:?} local {locals} bindings {}",
                    view.program_type,
                    view.keywords,
                    bindings.iter().map(short).collect::<Vec<_>>().join(", ")
                );
            } else if let Ok(params) = ProgramParameters::parse(bytes) {
                println!(
                    "  entry {i} param bindings {}",
                    params
                        .bindings
                        .iter()
                        .map(short)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                for scope in &params.constant_buffers {
                    println!("    scope {:?} size {}", scope.name, scope.size);
                    for c in &scope.constants {
                        println!(
                            "      {} {}x{} mat={} arr={} @{}",
                            c.name,
                            c.rows,
                            c.columns.unwrap_or(0),
                            c.is_matrix,
                            c.array_size,
                            c.offset
                        );
                    }
                }
            }
        }
    }
    Ok(())
}
