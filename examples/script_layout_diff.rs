//! Which of a game's scripts serialize differently on another engine: each MonoScript of
//! the game, its fields' tree generated against the game's engine assemblies and against
//! another engine's, the two compared by layout.
//!
//! usage: script_layout_diff <game dir> <other engine's Managed dir> <other version>
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rabex::UnityVersion;
use rabex::tpk::TpkTypeTreeBlob;
use rabex::typetree::TypeTreeNode;
use rabex::typetree::typetree_cache::sync::TypeTreeCache;
use rabex_convert::value::layout_equal;
use rabex_env::Environment;
use unity_typetree_gen::AssemblyTypeTreeGenerator;

/// Where two trees first differ, as a path.
fn first_difference(a: &TypeTreeNode, b: &TypeTreeNode, path: &str) -> String {
    if a.m_Type != b.m_Type || a.m_Name != b.m_Name || a.requires_align() != b.requires_align() {
        return format!(
            "{path}: {} {} -> {} {}",
            a.m_Type, a.m_Name, b.m_Type, b.m_Name
        );
    }
    for (x, y) in a.children.iter().zip(&b.children) {
        if !layout_equal(x, y) {
            return first_difference(x, y, &format!("{path}.{}", x.m_Name));
        }
    }
    let names = |n: &TypeTreeNode| {
        n.children
            .iter()
            .map(|c| c.m_Name.clone())
            .collect::<Vec<_>>()
    };
    format!("{path}: fields {:?} -> {:?}", names(a), names(b))
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let game = PathBuf::from(args.next().context("game dir")?);
    let other = PathBuf::from(args.next().context("other Managed dir")?);
    let other_version: UnityVersion = args.next().context("other version")?.parse()?;

    let env = Environment::new_in(&game, TypeTreeCache::new(TpkTypeTreeBlob::embedded()))?;
    let version = env.unity_version()?.clone();
    let managed = env.game_files.game_dir.join("Managed");
    // the game's assemblies on both sides; the engine's own from the side's engine
    let read = |dir: &Path, name: &str| std::fs::read(dir.join(name));
    let source = AssemblyTypeTreeGenerator::new(version.clone());
    let target = AssemblyTypeTreeGenerator::new(other_version.clone());

    let scripts = env.mono_scripts()?;
    let (mut same, mut differ, mut missing) = (0, Vec::new(), 0);
    for script in &scripts {
        let assembly = if script.m_AssemblyName.ends_with(".dll") {
            script.m_AssemblyName.clone()
        } else {
            format!("{}.dll", script.m_AssemblyName)
        };
        let a = source.generate(
            &|n| read(&managed, n),
            &assembly,
            &script.m_Namespace,
            &script.m_ClassName,
        );
        let b = target.generate(
            // the other engine's assemblies where it has them, the game's (its own code and
            // the packages it ships) otherwise
            &|n| read(&other, n).or_else(|_| read(&managed, n)),
            &assembly,
            &script.m_Namespace,
            &script.m_ClassName,
        );
        match (a, b) {
            (Ok(Some(a)), Ok(Some(b))) => match layout_equal(&a, &b) {
                true => same += 1,
                false => differ.push(format!(
                    "[{}] {}.{}: {}",
                    script.m_AssemblyName,
                    script.m_Namespace,
                    script.m_ClassName,
                    first_difference(&a, &b, "")
                )),
            },
            _ => missing += 1,
        }
    }
    println!(
        "{} scripts: {same} serialize alike on {version} and {other_version}, {} differ, {missing} not generated",
        scripts.len(),
        differ.len()
    );
    for line in &differ {
        println!("  {line}");
    }
    Ok(())
}
