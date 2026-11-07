//! Export every D3D11 program variant of an asset bundle's or a game's shaders as a DXBC container
//! with its `RDEF` chunk rebuilt from Unity's parameters, for feeding into DXBC translators.
//!
//! Usage: `shader_dxbc <bundle> <out>` or `shader_dxbc --game <name> <out>`

mod utils;

use std::io::Cursor;
use std::path::PathBuf;

use anyhow::{Context, Result};
use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig};
use rabex::objects::ClassId;
use rabex::typetree::NullTypeTreeProvider;
use rabex_shader::dxbc::{Container, rdef};
use rabex_shader::{Platform, Shader};

fn bundle_shaders(path: &PathBuf) -> Result<Vec<Shader>> {
    let bundle = BundleFileReader::from_reader(
        Cursor::new(std::fs::read(path)?),
        &ExtractionConfig::default(),
    )?;
    let entry = bundle
        .main_serializedfile()
        .context("bundle without serialized file")?;
    let data = bundle.read_at_entry(entry)?;
    let file = SerializedFile::from_reader(&mut Cursor::new(data.as_slice()))?;
    file.objects()
        .filter(|o| o.m_ClassID == ClassId::Shader)
        .map(|info| {
            Ok(file.read(
                info,
                NullTypeTreeProvider,
                &mut Cursor::new(data.as_slice()),
            )?)
        })
        .collect()
}

fn game_shaders(game: &str) -> Result<Vec<Shader>> {
    let env = utils::find_game(game)?.context("game not found")?;
    let mut files = env.load_all_serialized_files()?;
    for i in 0.. {
        let name = format!("sharedassets{i}.assets");
        let Ok(file) = env.load_serialized(&name) else {
            break;
        };
        files.insert(name, file);
    }
    let mut shaders = Vec::new();
    for file in files.values() {
        for info in file
            .file
            .objects()
            .filter(|o| o.m_ClassID == ClassId::Shader)
        {
            shaders.push(file.object_at(info.m_PathID)?.read()?);
        }
    }
    Ok(shaders)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (shaders, out) = match args.as_slice() {
        [flag, game, out] if flag == "--game" => (game_shaders(game)?, PathBuf::from(out)),
        [bundle, out] => (bundle_shaders(&PathBuf::from(bundle))?, PathBuf::from(out)),
        _ => anyhow::bail!("usage: shader_dxbc <bundle> <out> | --game <name> <out>"),
    };
    std::fs::create_dir_all(&out)?;

    let mut written = 0;
    let mut failures = Vec::new();
    for (shader_index, shader) in shaders.iter().enumerate() {
        let name = format!(
            "{}#{shader_index}",
            shader.m_ParsedForm.m_Name.replace('/', "_")
        );
        for blob in shader
            .platform_blobs()?
            .into_iter()
            .filter(|b| b.platform == Platform::D3D11)
        {
            for (s, sub_shader) in shader.m_ParsedForm.m_SubShaders.iter().enumerate() {
                for (p, pass) in sub_shader.m_Passes.iter().enumerate() {
                    for (v, variant) in shader.pass_variants(pass, &blob)?.into_iter().enumerate() {
                        let label = format!("{name}.{s}.{p}.{:?}.{v}", variant.stage);
                        let result = (|| -> Result<Vec<u8>> {
                            let mut container = Container::parse(variant.program.dxbc()?)?;
                            container.set_rdef(rdef(&variant.parameters, &container)?);
                            container.to_bytes()
                        })();
                        match result {
                            Ok(bytes) => {
                                std::fs::write(out.join(format!("{label}.dxbc")), bytes)?;
                                written += 1;
                            }
                            Err(e) => failures.push(format!("{label}: {e:#}")),
                        }
                    }
                }
            }
        }
    }
    println!("wrote {written} programs to {}", out.display());
    anyhow::ensure!(
        failures.is_empty(),
        "{} failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
    Ok(())
}
