//! Copies the MonoScripts a game file references in other files into it, and checks that every
//! MonoBehaviour of the result finds its script in the file itself.
//!
//! `cargo run --release --example inline_external -- silksong resources.assets`

mod utils;

use std::io::Cursor;

use anyhow::{Context, Result, ensure};
use rabex::files::SerializedFile;
use rabex::files::serializedfile::builder::SerializedFileBuilder;
use rabex::files::serializedfile::{self};
use rabex::objects::ClassId;
use rabex::serde_typetree;
use rabex::typetree::TypeTreeProvider;
use rabex_env::inline::inline_external;
use rabex_env::unity::types::MonoBehaviour;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let usage = "usage: inline_external <game> <file>";
    let game = args.next().context(usage)?;
    let path = args.next().context(usage)?;
    let env = utils::find_game(&game)?.with_context(|| format!("no game {game}"))?;

    let handle = env.load_serialized(&path)?;
    let version = env.unity_version()?;
    let common_offsets = serializedfile::build_common_offset_map(&env.tpk.inner, version);
    let mut builder = SerializedFileBuilder::from_serialized(
        version,
        handle.file,
        handle.data,
        &env.tpk,
        &common_offsets,
        handle.file.objects().cloned(),
    );
    env.fill_typetrees(&mut builder, handle.file, handle.data)?;
    // a file without typetrees keeps only nameless stubs of its [SerializeReference] types
    builder.serialized.m_RefTypes = Some(Vec::new());

    let inlined = inline_external(&env, &mut builder, &[ClassId::MonoScript])?;
    println!("{path}: copied {} objects in", inlined.len());
    for (pptr, copy) in inlined.iter().take(5) {
        let file = &builder.serialized.m_Externals[pptr.m_FileID.get_externals_index().unwrap()];
        println!("  {} {} -> {copy}", file.pathName, pptr.m_PathID);
    }

    let written = builder.write_vec()?;
    let file = SerializedFile::from_reader(&mut Cursor::new(&written))?;
    // the engine's MonoBehaviour fields, without the script's
    let header = env
        .tpk
        .get_typetree_node(ClassId::MonoBehaviour, version)
        .context("no MonoBehaviour typetree")?;
    let mut behaviours = 0;
    for info in file
        .objects()
        .filter(|info| info.m_ClassID == ClassId::MonoBehaviour)
    {
        let start = info.m_Offset as usize;
        let behaviour: MonoBehaviour = serde_typetree::from_reader_endianed(
            &mut Cursor::new(&written[start..]),
            &header,
            file.m_Header.m_Endianess,
        )?;
        let script = behaviour.m_Script;
        ensure!(
            script.is_local() && file.get_object_info(script.m_PathID).is_some(),
            "MonoBehaviour {} still refers to {script:?}",
            info.m_PathID
        );
        behaviours += 1;
    }
    println!("all {behaviours} MonoBehaviours find their script in the file");
    Ok(())
}
