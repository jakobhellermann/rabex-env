//! Compares the rust typetree generator to `TypeTreeGeneratorAPI` on
//! every installed steam game.
use std::collections::BTreeSet;

use anyhow::Result;
use rabex::objects::ClassId;
use rabex::typetree::{TypeTreeNode, TypeTreeProvider};
use rabex_env::Environment;
use rabex_env::game_files::GameFiles;
use typetree_generator_api::{GeneratorBackend, TypeTreeGenerator};

mod utils;

struct NativeGen {
    generator: TypeTreeGenerator,
    base_node: TypeTreeNode,
}

impl NativeGen {
    fn build<P: TypeTreeProvider>(env: &Environment<GameFiles, P>) -> Result<Self> {
        let unity_version = env.unity_version()?;
        let generator =
            TypeTreeGenerator::new_lib_next_to_exe(unity_version, GeneratorBackend::AssetsTools)?;
        // The cache prepends the MonoBehaviour base nodes itself, so disable the
        // native lib's own root-node insertion (otherwise the header is doubled).
        generator.set_add_monobehaviour_root_nodes(false)?;
        generator.load_all_dll_in_dir(env.game_files.game_dir.join("Managed"))?;
        let base_node = env
            .tpk
            .get_typetree_node(ClassId::MonoBehaviour, unity_version)
            .expect("missing MonoBehaviour class")
            .into_owned();
        Ok(NativeGen {
            generator,
            base_node,
        })
    }

    fn generate(&self, assembly: &str, full_name: &str) -> Result<Option<TypeTreeNode>> {
        Ok(self
            .generator
            .generate_typetree_raw(self.base_node.clone(), assembly, full_name)?)
    }
}

#[derive(Default)]
struct Totals {
    games: usize,
    scripts: usize,
    both_ok: usize,
    mismatches: usize,
    rust_only: usize,
    native_only: usize,
    both_failed: usize,
}

fn main() -> Result<()> {
    let mut totals = Totals::default();
    let only = std::env::var("ONLY_GAME").ok();

    utils::for_each_steam_game(|env| {
        // Use the name when available; fall back to the data dir.
        let name = env
            .app_info()
            .map(|a| a.name)
            .unwrap_or_else(|_| env.game_files.game_dir.display().to_string());

        if let Some(filter) = &only
            && !name.to_lowercase().contains(&filter.to_lowercase())
        {
            return Ok(());
        }

        eprintln!(">>> {name} ({})", env.game_files.game_dir.display());
        match compare_game(env, &mut totals) {
            Ok(Some((scripts, ok, mm, ro, no, bf))) => {
                println!(
                    "[{name}] {scripts} scripts: {ok} ok, {mm} mismatch, \
                     {ro} rust-only, {no} native-only, {bf} both-failed"
                );
                totals.games += 1;
            }
            Ok(None) => {
                eprintln!("[{name}] skipped (no comparable MonoBehaviours)");
            }
            Err(e) => eprintln!("[{name}] error: {e:#}"),
        }
        Ok(())
    })?;

    println!(
        "\n=== totals: {} games, {} scripts, {} ok, {} mismatches, \
         {} rust-only, {} native-only, {} both-failed ===",
        totals.games,
        totals.scripts,
        totals.both_ok,
        totals.mismatches,
        totals.rust_only,
        totals.native_only,
        totals.both_failed,
    );

    assert_eq!(
        totals.mismatches, 0,
        "{} type tree mismatches between backends",
        totals.mismatches
    );
    Ok(())
}

/// Returns per-game counts, or `None` if the game has no comparable scripts.
fn compare_game<P: TypeTreeProvider>(
    env: Environment<GameFiles, P>,
    totals: &mut Totals,
) -> Result<Option<(usize, usize, usize, usize, usize, usize)>> {
    let scripts = match env.mono_scripts() {
        Ok(s) if !s.is_empty() => s,
        _ => return Ok(None),
    };

    let native_gen = match NativeGen::build(&env) {
        Ok(n) => n,
        Err(_) => return Ok(None),
    };

    let mut both_ok = 0;
    let mut mismatches = 0;
    let mut rust_only = 0;
    let mut native_only = 0;
    let mut both_failed = 0;
    let mut seen: BTreeSet<String> = BTreeSet::new();

    for script in &scripts {
        let assembly = script.assembly_name();
        let full_name = script.full_name();
        let label = format!("{full_name} ({assembly})");
        if !seen.insert(label.clone()) {
            continue;
        }

        match (
            env.generate_typetree(&assembly, &full_name),
            native_gen.generate(&assembly, &full_name),
        ) {
            (Ok(Some(rust_tt)), Ok(Some(native_tt))) => {
                both_ok += 1;
                if *rust_tt != native_tt {
                    mismatches += 1;
                    let diff = first_diff(rust_tt, &native_tt, String::new())
                        .unwrap_or_else(|| "  <equal node-by-node, but != ?>".to_string());
                    eprintln!("MISMATCH {label}:\n{diff}");
                }
            }
            // Both agree the type isn't present — not a discrepancy.
            (Ok(None), Ok(None)) => both_ok += 1,
            // rust produced a tree, native didn't (not-found or error).
            (Ok(Some(_)), Ok(None)) | (Ok(Some(_)), Err(_)) => {
                rust_only += 1;
                eprintln!("native missing (rust produced a tree): {label}");
            }
            // native produced a tree, rust didn't.
            (Ok(None), Ok(Some(_))) | (Err(_), Ok(Some(_))) => {
                native_only += 1;
                eprintln!("rust missing (native produced a tree): {label}");
            }
            (Err(_), Err(_)) | (Ok(None), Err(_)) | (Err(_), Ok(None)) => both_failed += 1,
        }
    }

    totals.scripts += seen.len();
    totals.both_ok += both_ok;
    totals.mismatches += mismatches;
    totals.rust_only += rust_only;
    totals.native_only += native_only;
    totals.both_failed += both_failed;

    Ok(Some((
        seen.len(),
        both_ok,
        mismatches,
        rust_only,
        native_only,
        both_failed,
    )))
}

/// Walks both trees in lockstep and returns a description of the first node
/// that differs, instead of dumping the whole (huge) `Debug` representation.
fn first_diff(a: &TypeTreeNode, b: &TypeTreeNode, path: String) -> Option<String> {
    let here = if path.is_empty() {
        a.m_Name.clone()
    } else {
        format!("{path}.{}", a.m_Name)
    };

    if a.m_Name != b.m_Name
        || a.m_Type != b.m_Type
        || a.m_Level != b.m_Level
        || a.m_ByteSize != b.m_ByteSize
        || a.m_TypeFlags != b.m_TypeFlags
        || a.m_MetaFlag != b.m_MetaFlag
        || a.children.len() != b.children.len()
    {
        return Some(format!(
            "  at `{here}`:\n    rust:   {} {} (lvl {}, meta {:?}, {} children)\n    native: {} {} (lvl {}, meta {:?}, {} children)",
            a.m_Type,
            a.m_Name,
            a.m_Level,
            a.m_MetaFlag,
            a.children.len(),
            b.m_Type,
            b.m_Name,
            b.m_Level,
            b.m_MetaFlag,
            b.children.len(),
        ));
    }

    for (ca, cb) in a.children.iter().zip(&b.children) {
        if let Some(d) = first_diff(ca, cb, here.clone()) {
            return Some(d);
        }
    }
    None
}
