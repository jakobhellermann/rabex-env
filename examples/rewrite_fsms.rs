//! Rewrites FIELDS of every SCRIPT in the given scenes or files.
//!
//! `cargo run --release --example rewrite_fsms -- 'Hollow Knight' Ruins1_03 resources.assets`

mod utils;

use std::borrow::Cow;

use anyhow::{Context, Result};
use rabex_env::rewrite::{FieldPath, FieldRewriter, FieldValue};
use rabex_env::unity::types::MonoBehaviour;

const SCRIPT: &str = "PlayMakerFSM";
const FIELDS: &[(&str, FieldValue)] = &[
    ("fsm.preprocessed", FieldValue::Bool(false)),
    ("eventHandlerComponentsAdded", FieldValue::Bool(false)),
];

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let game = args
        .next()
        .context("usage: rewrite_fsms <game> <scene or file>...")?;
    let env = utils::find_game(&game)?.with_context(|| format!("no game {game}"))?;
    let scenes = env.build_settings()?.scene_name_lookup();

    let fields = FIELDS
        .iter()
        .map(|&(path, value)| Ok((path.parse()?, value)))
        .collect::<Result<Vec<(FieldPath, FieldValue)>>>()?;
    let mut rewriter = None;

    for name in args {
        let file = match scenes.get(&name) {
            Some(index) => format!("level{index}"),
            None => name.clone(),
        };
        let handle = env.load_serialized(&file)?;

        let (mut objects, mut changed) = (0, 0);
        for object in handle.objects_of::<MonoBehaviour>() {
            let Some(script) = object.mono_script()? else {
                continue;
            };
            if script.full_name() != SCRIPT {
                continue;
            }
            let rewriter = match &rewriter {
                Some(rewriter) => rewriter,
                None => rewriter.insert(FieldRewriter::new(
                    env.generate_typetree_for_script(&script)?,
                    &fields,
                )?),
            };

            let mut data = Cow::Borrowed(object.data());
            rewriter.apply(&mut data)?;
            objects += 1;
            changed += matches!(data, Cow::Owned(_)) as usize;
        }
        println!("{name} ({file}): rewrote {changed} of {objects} {SCRIPT}s");
    }
    Ok(())
}
