//! Rewrites FIELDS of every SCRIPT in the given scenes or files.
//!
//! `cargo run --release --example rewrite_fsms -- 'Hollow Knight' Ruins1_03 resources.assets`

mod utils;

use std::borrow::Cow;

use anyhow::{Context, Result};
use rabex_env::rewrite::{FieldPath, Fields, Scalar};
use rabex_env::unity::types::MonoBehaviour;

const SCRIPT: &str = "PlayMakerFSM";
const FIELDS: &[(&str, Scalar)] = &[
    ("fsm.preprocessed", Scalar::Bool(false)),
    ("eventHandlerComponentsAdded", Scalar::Bool(false)),
];

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let game = args
        .next()
        .context("usage: rewrite_fsms <game> <scene or file>...")?;
    let env = utils::find_game(&game)?.with_context(|| format!("no game {game}"))?;
    let scenes = env.build_settings()?.scene_name_lookup();

    let paths = FIELDS
        .iter()
        .map(|(path, _)| path.parse())
        .collect::<Result<Vec<FieldPath>>>()?;
    let values: Vec<Scalar> = FIELDS.iter().map(|(_, value)| *value).collect();
    let mut fields = None;

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
            let fields = match &fields {
                Some(fields) => fields,
                None => fields.insert(Fields::at(
                    env.generate_typetree_for_script(&script)?,
                    &paths,
                )?),
            };

            let mut data = Cow::Borrowed(object.data());
            fields.rewrite(&mut data, |current| {
                current.copy_from_slice(&values);
                Ok(())
            })?;
            objects += 1;
            changed += matches!(data, Cow::Owned(_)) as usize;
        }
        println!("{name} ({file}): rewrote {changed} of {objects} {SCRIPT}s");
    }
    Ok(())
}
