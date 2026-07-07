//! The jq engine: compiling a query and running it over enriched object values, with a native
//! `deref` filter that follows a qualified PPtr through the [`Environment`].

use anyhow::{Context as _, Result, anyhow};
use core::marker::PhantomData;
use jaq_core::{Cv, DataT, Filter, Lut, ValXs, Vars, data, load, unwrap_valr};
use jaq_json::Val;
use jaq_std::input::{self, Inputs};
use rabex_env::Environment;
use rabex_env::rabex::objects::PPtr;
use rabex_env::rabex::tpk::TpkTypeTreeBlob;
use rabex_env::rabex::typetree::TypeTreeProvider;
use rabex_env::rabex::typetree::typetree_cache::sync::TypeTreeCache;
use rabex_env::resolver::{EnvResolver, GameFiles};

use crate::enrich::{Enrich, enrich};
use crate::pptr::QualifiedPPtr;

/// Capability trait giving a jaq run's context access to the [`Environment`], so the native `deref`
/// filter can resolve PPtrs without a process-global. Mirrors how jaq-core exposes the `Lut` via
/// `HasLut` and jaq-std the inputs via `HasInputs`: the env is threaded through the filter's `Ctx`
/// (`DataT::Data`), never captured — a `Native` is a bare `fn` pointer and cannot close over it.
pub trait HasEnv<'a, R, P> {
    fn env(&self) -> &'a Environment<R, P>;
}

fn deref<R: EnvResolver, P: TypeTreeProvider>(env: &Environment<R, P>, pptr: Val) -> Result<Val> {
    let qualified = QualifiedPPtr::from_val(&pptr)?;

    let file = env
        .load_serialized(&qualified.file)
        .with_context(|| format!("Failed to load '{}'", qualified.file))?;
    let target = file.deref(PPtr::local(qualified.path_id).typed::<Val>())?;
    let mut value = target.read().map_err(|e| {
        anyhow!(
            "Failed to read object {} in {}: {e}",
            qualified.path_id,
            qualified.file
        )
    })?;

    let script = target.mono_script()?;
    enrich(
        &mut value,
        &qualified.file,
        &file,
        Enrich {
            scenes: None,
            script: script.as_ref(),
        },
    )?;
    Ok(value)
}

// Pulling the body into a generic fn with a *named* `'a` (rather than inlining in the closure) is
// what makes `ctx.data().env()` unambiguous — exactly how jaq-std's `inputs` reaches its data.
fn deref_native<'a, R, P>(cv: Cv<'a, DataKind<R, P>>) -> ValXs<'a, Val>
where
    R: EnvResolver + 'static,
    P: TypeTreeProvider + 'static,
{
    let (ctx, val) = cv;
    let env = ctx.data().env();
    let obj = deref(env, val).map_err(|e| {
        jaq_core::Exn::from(jaq_core::Error::str(format!("Cannot call `deref`: {e}")))
    });
    Box::new(core::iter::once(obj))
}

fn funs<R, P>() -> impl Iterator<Item = jaq_core::native::Fun<DataKind<R, P>>>
where
    R: EnvResolver + 'static,
    P: TypeTreeProvider + 'static,
{
    [(
        "deref",
        vec![].into_boxed_slice(),
        jaq_core::Native::new(|cv| deref_native::<R, P>(cv)),
    )]
    .into_iter()
}

/// A compiled jq query, ready to run over object values via [`QueryRunner::exec`]. Generic over the
/// resolver/provider with defaults matching [`Environment`], so callers name neither in practice.
pub struct QueryRunner<R = GameFiles, P = TypeTreeCache<TpkTypeTreeBlob>>
where
    R: 'static,
    P: 'static,
{
    filter: Filter<DataKind<R, P>>,
}

impl<R: EnvResolver + 'static, P: TypeTreeProvider + 'static> QueryRunner<R, P> {
    pub fn set_query(&mut self, query: &str) -> Result<()> {
        *self = QueryRunner::new(query)?;
        Ok(())
    }

    pub fn new(query: &str) -> Result<Self> {
        let jq_defs = load::parse(include_str!("defs.jq"), |p| p.defs()).unwrap();

        let loader = load::Loader::new(
            jaq_core::defs()
                .chain(jaq_std::defs())
                .chain(jaq_json::defs())
                .chain(jq_defs),
        );

        let program = load::File {
            code: query,
            path: (),
        };
        let arena = load::Arena::default();
        let modules = loader.load(&arena, program).map_err(load_errors)?;

        let filter = jaq_core::Compiler::default()
            .with_funs(
                jaq_core::funs()
                    .chain(jaq_std::funs())
                    .chain(jaq_json::funs())
                    .chain(funs::<R, P>()),
            )
            .compile(modules)
            .map_err(|errors| {
                let mut text = String::new();
                for (_, all) in errors {
                    for (found, undefined) in all {
                        text.push_str(&format!("undefined {}: {}\n", undefined.as_str(), found));
                    }
                }
                text.truncate(text.len().saturating_sub(1));
                anyhow!("{text}")
            })?;

        Ok(QueryRunner { filter })
    }

    /// Run the query over one object `item`, resolving `deref` against `env`. Returns every value
    /// the query yields for that input.
    pub fn exec(&self, env: &Environment<R, P>, item: Val) -> Result<Vec<Val>> {
        let inputs = jaq_std::input::RcIter::new(core::iter::empty());
        let data = Data {
            lut: &self.filter.lut,
            inputs: &inputs,
            env,
        };
        let out = self.filter.id.run::<DataKind<R, P>>((
            jaq_core::Ctx::new(&data, Vars::new(core::iter::empty())),
            item,
        ));
        unwrap_valr(out.collect::<Result<Vec<_>, _>>()).map_err(|e| anyhow!("{e}"))
    }
}

fn load_errors(errors: Vec<(load::File<&str, ()>, load::Error<&str>)>) -> anyhow::Error {
    let mut text = String::new();
    for (_, error) in errors {
        match error {
            load::Error::Io(items) => {
                for (path, error) in items {
                    text.push_str(&format!("could not load file {path}: {error}\n"));
                }
            }
            load::Error::Lex(items) => {
                for (expected, found) in items {
                    text.push_str(&format!("expected {}, found {found}\n", expected.as_str()));
                }
            }
            load::Error::Parse(items) => {
                for (expected, found) in items {
                    let found = if found.is_empty() {
                        "unexpected end of input"
                    } else {
                        found
                    };
                    text.push_str(&format!("expected {}, found {found}\n", expected.as_str()));
                }
            }
        }
    }
    text.truncate(text.len().saturating_sub(1));
    anyhow!("{text}")
}

// `DataT` must be `'static`, so the resolver/provider ride along as `PhantomData` type params
// rather than borrowed values; the actual `&Environment<R, P>` lives in `Data`, threaded in per
// `exec` call. Defaults mirror `Environment`'s, so callers name neither.
pub struct DataKind<R = GameFiles, P = TypeTreeCache<TpkTypeTreeBlob>>(PhantomData<fn() -> (R, P)>);

impl<R: 'static, P: 'static> DataT for DataKind<R, P> {
    type V<'a> = Val;
    type Data<'a> = &'a Data<'a, R, P>;
}

pub struct Data<'a, R = GameFiles, P = TypeTreeCache<TpkTypeTreeBlob>>
where
    R: 'static,
    P: 'static,
{
    lut: &'a Lut<DataKind<R, P>>,
    inputs: Inputs<'a, Val>,
    env: &'a Environment<R, P>,
}

impl<'a, R: 'static, P: 'static> data::HasLut<'a, DataKind<R, P>> for &'a Data<'a, R, P> {
    fn lut(&self) -> &'a Lut<DataKind<R, P>> {
        self.lut
    }
}

impl<'a, R: 'static, P: 'static> HasEnv<'a, R, P> for &'a Data<'a, R, P> {
    fn env(&self) -> &'a Environment<R, P> {
        self.env
    }
}

impl<'a, R: 'static, P: 'static> input::HasInputs<'a, Val> for &'a Data<'a, R, P> {
    fn inputs(&self) -> Inputs<'a, Val> {
        self.inputs
    }
}

#[cfg(test)]
mod tests {
    use super::QueryRunner;
    use jaq_json::Val;
    use rabex_env::Environment;
    use rabex_env::resolver::MemResolver;

    fn val(s: &str) -> Val {
        jaq_json::read::parse_single(s.as_bytes()).unwrap()
    }

    fn tpk() -> rabex_env::rabex::typetree::typetree_cache::sync::TypeTreeCache<
        rabex_env::rabex::tpk::TpkTypeTreeBlob,
    > {
        rabex_env::rabex::typetree::typetree_cache::sync::TypeTreeCache::new(
            rabex_env::rabex::tpk::TpkTypeTreeBlob::embedded(),
        )
    }

    /// Env-free queries: an empty in-memory env is enough because they never resolve a PPtr.
    fn run(query: &str, input: &str) -> Vec<Val> {
        let env = Environment::new(MemResolver::new(), tpk());
        let runner = QueryRunner::new(query).unwrap();
        runner.exec(&env, val(input)).unwrap()
    }

    #[test]
    fn plain_field_access() {
        assert_eq!(run(".a", r#"{"a": 1, "b": 2}"#), vec![val("1")]);
    }

    #[test]
    fn builtin_defs_available() {
        // `name` comes from defs.jq.
        assert_eq!(run("name", r#"{"m_Name": "Hero"}"#), vec![val(r#""Hero""#)]);
        assert_eq!(
            run(".[] | nonnull", "[1, null, 2]"),
            vec![val("1"), val("2")]
        );
    }

    #[test]
    fn invalid_query_is_a_compile_error() {
        let result: anyhow::Result<QueryRunner> = QueryRunner::new(".[");
        assert!(result.is_err());
    }

    /// `deref` resolves a qualified PPtr through the env passed to `exec`. The `GameFiles` resolver
    /// wants a real dir, so the fixture is staged as `<tmp>/Game_Data/level0`.
    #[test]
    fn deref_follows_a_qualified_pptr() {
        use rabex_env::resolver::GameFiles;
        use rabex_env_testkit::Flat;

        let (bytes, go_ids) = Flat::new(&["Player"]).write();
        let tmp = tempfile::TempDir::new().unwrap();
        let data_dir = tmp.path().join("Game_Data");
        std::fs::create_dir(&data_dir).unwrap();
        std::fs::write(data_dir.join("level0"), bytes).unwrap();

        let env = Environment::new(GameFiles::probe(tmp.path()).unwrap(), tpk());
        let runner = QueryRunner::new("deref | .m_Name").unwrap();
        let pptr = val(&format!(
            r#"{{ "file": "level0", "path_id": {} }}"#,
            go_ids[0]
        ));
        assert_eq!(runner.exec(&env, pptr).unwrap(), vec![val(r#""Player""#)]);
    }
}
