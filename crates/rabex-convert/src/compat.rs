//! Making objects readable by an older Unity without changing their layout.
//!
//! A bundle carries its type trees, and a player reads its objects by them, field by
//! field and name by name: a field the player doesn't know is skipped, one it misses keeps
//! its default. What that can't get right are the fields a [`crate::fields`] rule decides,
//! like a StreamedClip's `curveCount`, which Unity 2022 split in two. Where a rule's value
//! fits the source field, it is written in place and the fields it folds are zeroed; the
//! changed bytes are byte patches against the unchanged file.
//!
//! Rules that need another layout (a renamed field, a field the source lacks) can't be
//! applied in place and are reported in [`Compat::unfixable`].
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::io::Cursor;

use anyhow::{Context, Result, bail, ensure};
use rabex::UnityVersion;
use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileReader, CompressionType, ExtractionConfig};
use rabex::objects::ClassId;
use rabex::typetree::{TypeTreeNode, TypeTreeProvider, TypetreeNodeKind as Kind};
use rabex_env::rewrite::{FieldPath, Fields, Scalar};
use std::collections::HashMap;
use std::collections::hash_map::Entry;

use crate::fields::{Rule, Source, rule};
use crate::value::Value;

/// The in-place fixes objects of one type tree need to be read as an older version.
pub struct Compat {
    owners: Vec<Owner>,
    unfixable: Vec<String>,
}

/// A struct type with fields a rule decides.
struct Owner {
    /// Every scalar field of the struct, which rules read from.
    fields: Fields,
    kinds: Vec<(String, Kind)>,
    edits: Vec<Edit>,
}

struct Edit {
    /// The field the rule writes, by its index into [`Owner::kinds`].
    field: usize,
    rule: Rule,
    folds: Vec<usize>,
}

impl Compat {
    /// What objects of the `source` tree need to be read by a player of the `target` tree;
    /// `None` if they need nothing.
    pub fn plan(source: &TypeTreeNode, target: &TypeTreeNode) -> Result<Option<Compat>> {
        let mut types = BTreeSet::new();
        struct_types(source, &mut types);

        let (mut owners, mut unfixable) = (Vec::new(), Vec::new());
        for ty in types {
            let (Some(from), Some(to)) = (find_type(source, ty), find_type(target, ty)) else {
                continue;
            };
            if let Some(owner) = plan_owner(source, from, to, &mut unfixable)
                .with_context(|| format!("{ty} from {} to {}", source.m_Type, target.m_Type))?
            {
                owners.push(owner);
            }
        }

        Ok((!owners.is_empty() || !unfixable.is_empty()).then_some(Compat { owners, unfixable }))
    }

    /// The rules that apply but can't be written in place, as `Type.field: why`.
    pub fn unfixable(&self) -> &[String] {
        &self.unfixable
    }

    /// Rewrites `object` for `target`; it is copied only once a byte changes.
    pub fn apply(&self, object: &mut Cow<'_, [u8]>, target: &UnityVersion) -> Result<()> {
        for owner in &self.owners {
            owner.fields.rewrite(object, |values| {
                let source = InPlace {
                    kinds: &owner.kinds,
                    values: values.to_vec(),
                    target,
                    warnings: RefCell::default(),
                };

                for edit in &owner.edits {
                    let (name, kind) = &owner.kinds[edit.field];
                    let value = edit.rule.value(&source).with_context(|| name.clone())?;
                    values[edit.field] = scalar(&value, kind).with_context(|| name.clone())?;
                    for &fold in &edit.folds {
                        values[fold] = zero(&owner.kinds[fold].1);
                    }
                }

                let warnings = source.warnings.take();
                ensure!(warnings.is_empty(), "inexact: {}", warnings.join("; "));
                Ok(())
            })?;
        }
        Ok(())
    }
}

fn plan_owner(
    root: &TypeTreeNode,
    from: &TypeTreeNode,
    to: &TypeTreeNode,
    unfixable: &mut Vec<String>,
) -> Result<Option<Owner>> {
    let kinds: Vec<(String, Kind)> = from
        .children
        .iter()
        .filter(|child| is_scalar(child))
        .map(|child| (child.m_Name.clone(), child.classify()))
        .collect();
    let index = |name: &str| kinds.iter().position(|(n, _)| n == name);

    let mut edits = Vec::new();
    for field in &to.children {
        let name = field.m_Name.as_str();
        let Some(rule) = rule(from, to, name) else {
            continue;
        };
        let why = match (rule, index(name)) {
            (Rule::Renamed { from, .. }, _) => format!("renamed from {from}"),
            (Rule::Value(..), None) => "the source has no scalar field to write it into".into(),
            (Rule::Value(..), Some(field)) => {
                let folds = rule
                    .replaces()
                    .iter()
                    .map(|fold| index(fold).with_context(|| format!("folds non-scalar {fold}")))
                    .collect::<Result<_>>()?;
                edits.push(Edit { field, rule, folds });
                continue;
            }
        };
        unfixable.push(format!("{}.{name}: {why}", to.m_Type));
    }

    if edits.is_empty() {
        return Ok(None);
    }

    let names: Vec<&str> = kinds.iter().map(|(name, _)| name.as_str()).collect();
    let fields = if std::ptr::eq(root, from) {
        let paths: Vec<FieldPath> = names
            .iter()
            .map(|name| name.parse())
            .collect::<Result<_>>()?;
        Fields::at(root, &paths)?
    } else {
        Fields::of_type(root, &from.m_Type, &names)?.context("the type is in the tree")?
    };

    Ok(Some(Owner {
        fields,
        kinds,
        edits,
    }))
}

/// Byte patches that make the objects of `bundle` readable by `target`, and the rules
/// that apply but can't be written in place. `source` is what to read the bundle as when
/// it names no version, as Addressables bundles don't.
pub fn bundle_patches<P: TypeTreeProvider>(
    bundle: &[u8],
    source: &UnityVersion,
    target: &UnityVersion,
    typetrees: &P,
) -> Result<BundlePatches> {
    let config = ExtractionConfig::default().with_fallback_unity_version(source.clone());
    let reader = BundleFileReader::from_reader(Cursor::new(bundle), &config)?;
    ensure!(
        reader.compression() == CompressionType::None,
        "compressed data can't be patched in place"
    );

    let mut out = BundlePatches::default();
    for entry in reader.serialized_files() {
        let data = reader.read_entry(entry)?;
        let file = SerializedFile::from_reader_with_version(&mut Cursor::new(&*data), source)?;

        let mut plans: HashMap<i32, Option<Compat>> = HashMap::new();
        for info in file.objects() {
            // scripts' trees have no counterpart in the target's type tree package
            if info.m_ClassID == ClassId::MonoBehaviour {
                continue;
            }

            if let Entry::Vacant(slot) = plans.entry(info.m_TypeID) {
                let from = file.get_typetree_for(info, typetrees)?;
                let to = typetrees
                    .get_typetree_node(info.m_ClassID, target)
                    .with_context(|| {
                        format!("{:?} has no type tree for {target}", info.m_ClassID)
                    })?;

                let compat = Compat::plan(&from, &to)?;
                if let Some(compat) = &compat {
                    out.unfixable.extend(compat.unfixable.iter().cloned());
                }
                slot.insert(compat);
            }

            let Some(compat) = &plans[&info.m_TypeID] else {
                continue;
            };

            let start = info.m_Offset as usize;
            let original = &data[start..start + info.m_Size as usize];
            let mut object = Cow::Borrowed(original);
            compat
                .apply(&mut object, target)
                .with_context(|| format!("object {} of {}", info.m_PathID, entry.path))?;

            let base = entry.offset as u64 + start as u64;
            out.patches
                .extend(diff(original, &object).map(|(offset, bytes)| Patch {
                    offset: base + offset as u64,
                    bytes: bytes.to_vec(),
                }));
        }
    }

    out.patches.sort_by_key(|patch| patch.offset);
    Ok(out)
}

#[derive(Debug, Default)]
pub struct BundlePatches {
    /// In the order of their offsets.
    pub patches: Vec<Patch>,
    /// See [`Compat::unfixable`].
    pub unfixable: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Patch {
    /// From the start of the bundle's data.
    pub offset: u64,
    pub bytes: Vec<u8>,
}

/// The runs of bytes `new` changes against `old`, by their offset.
fn diff<'a>(old: &'a [u8], new: &'a [u8]) -> impl Iterator<Item = (usize, &'a [u8])> + 'a {
    let mut i = 0;
    std::iter::from_fn(move || {
        while i < old.len() && old[i] == new[i] {
            i += 1;
        }
        let start = i;
        while i < old.len() && old[i] != new[i] {
            i += 1;
        }
        (start < i).then(|| (start, &new[start..i]))
    })
}

/// The values of a struct's scalar fields, as the rules read them.
struct InPlace<'a> {
    kinds: &'a [(String, Kind)],
    values: Vec<Scalar>,
    target: &'a UnityVersion,
    warnings: RefCell<Vec<String>>,
}

impl Source for InPlace<'_> {
    fn field(&self, name: &str) -> Result<Option<Value>> {
        let Some(index) = self.kinds.iter().position(|(n, _)| n == name) else {
            return Ok(None);
        };
        value(self.values[index], &self.kinds[index].1).map(Some)
    }

    fn object(&self) -> Result<&Value> {
        bail!("rules applied in place read only the scalar fields of their struct")
    }

    fn index(&self) -> Result<usize> {
        bail!("rules applied in place read only the scalar fields of their struct")
    }

    fn target(&self) -> &UnityVersion {
        self.target
    }

    fn warn(&self, message: String) {
        self.warnings.borrow_mut().push(message);
    }
}

fn is_scalar(node: &TypeTreeNode) -> bool {
    matches!(
        node.classify(),
        Kind::Bool
            | Kind::Char
            | Kind::U8
            | Kind::I8
            | Kind::U16
            | Kind::I16
            | Kind::U32
            | Kind::I32
            | Kind::U64
            | Kind::I64
            | Kind::Float
            | Kind::Double
    )
}

/// `scalar` as the value a field of `kind` deserializes to.
fn value(scalar: Scalar, kind: &Kind) -> Result<Value> {
    let int = |scalar: Scalar| match scalar {
        Scalar::Int(n) => Ok(n),
        other => bail!("{other:?} for an integer field"),
    };
    Ok(match (kind, scalar) {
        (Kind::Bool, Scalar::Bool(b)) => Value::Bool(b),
        (Kind::Float, Scalar::Float(x)) => Value::F32(x as f32),
        (Kind::Double, Scalar::Float(x)) => Value::F64(x),
        (Kind::U8 | Kind::Char, _) => Value::U8(int(scalar)?.try_into()?),
        (Kind::I8, _) => Value::I8(int(scalar)?.try_into()?),
        (Kind::U16, _) => Value::U16(int(scalar)?.try_into()?),
        (Kind::I16, _) => Value::I16(int(scalar)?.try_into()?),
        (Kind::U32, _) => Value::U32(int(scalar)?.try_into()?),
        (Kind::I32, _) => Value::I32(int(scalar)?.try_into()?),
        (Kind::U64, _) => Value::U64(int(scalar)?.try_into()?),
        (Kind::I64, _) => Value::I64(int(scalar)?),
        (kind, scalar) => bail!("{scalar:?} for a {kind:?} field"),
    })
}

/// A rule's `value` to write into a field of `kind`.
fn scalar(value: &Value, kind: &Kind) -> Result<Scalar> {
    Ok(match *value {
        Value::Bool(b) => Scalar::Bool(b),
        Value::U8(n) => Scalar::Int(n.into()),
        Value::I8(n) => Scalar::Int(n.into()),
        Value::U16(n) => Scalar::Int(n.into()),
        Value::I16(n) => Scalar::Int(n.into()),
        Value::U32(n) => Scalar::Int(n.into()),
        Value::I32(n) => Scalar::Int(n.into()),
        Value::U64(n) => Scalar::Int(n.try_into()?),
        Value::I64(n) => Scalar::Int(n),
        Value::F32(x) => Scalar::Float(x.into()),
        Value::F64(x) => Scalar::Float(x),
        ref other => bail!("{other:?} for a {kind:?} field"),
    })
}

fn zero(kind: &Kind) -> Scalar {
    match kind {
        Kind::Bool => Scalar::Bool(false),
        Kind::Float | Kind::Double => Scalar::Float(0.0),
        _ => Scalar::Int(0),
    }
}

fn struct_types<'a>(node: &'a TypeTreeNode, types: &mut BTreeSet<&'a str>) {
    if matches!(node.classify(), Kind::Struct) {
        types.insert(&node.m_Type);
    }
    for child in &node.children {
        struct_types(child, types);
    }
}

fn find_type<'a>(node: &'a TypeTreeNode, ty: &str) -> Option<&'a TypeTreeNode> {
    if node.m_Type == ty && matches!(node.classify(), Kind::Struct) {
        return Some(node);
    }
    node.children.iter().find_map(|child| find_type(child, ty))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(ty: &str, name: &str, children: Vec<TypeTreeNode>) -> TypeTreeNode {
        TypeTreeNode {
            m_Type: ty.to_owned(),
            m_Name: name.to_owned(),
            m_MetaFlag: Some(0),
            children,
            ..Default::default()
        }
    }

    fn leaf(ty: &str, name: &str) -> TypeTreeNode {
        node(ty, name, vec![])
    }

    fn vector(name: &str, element: TypeTreeNode) -> TypeTreeNode {
        node(
            "vector",
            name,
            vec![node("Array", "Array", vec![leaf("int", "size"), element])],
        )
    }

    /// An AnimationClip with a StreamedClip, in Unity 6's form and in 2020's.
    fn clips() -> (TypeTreeNode, TypeTreeNode) {
        let streamed = |counts: Vec<TypeTreeNode>| {
            let mut fields = vec![vector("data", leaf("UInt32", "data"))];
            fields.extend(counts);
            node("StreamedClip", "m_StreamedClip", fields)
        };
        let clip = |streamed| {
            node(
                "AnimationClip",
                "Base",
                vec![leaf("int", "m_Before"), streamed],
            )
        };
        let source = clip(streamed(vec![
            leaf("UInt16", "curveCount"),
            leaf("UInt16", "discreteCurveCount"),
        ]));
        let target = clip(streamed(vec![leaf("UInt32", "curveCount")]));
        (source, target)
    }

    fn v2020() -> UnityVersion {
        "2020.2.2f1".parse().unwrap()
    }

    #[test]
    fn discrete_curves_fold_into_the_curve_count_in_place() -> Result<()> {
        let (source, target) = clips();
        let compat = Compat::plan(&source, &target)?.expect("the clip needs fixing");
        assert!(compat.unfixable().is_empty());

        #[rustfmt::skip]
        let object = [
            9, 0, 0, 0,             // m_Before
            1, 0, 0, 0, 7, 0, 0, 0, // data: [7]
            3, 0, 2, 0,             // curveCount 3, discreteCurveCount 2
        ];
        let mut patched = Cow::Borrowed(&object[..]);
        compat.apply(&mut patched, &v2020())?;
        #[rustfmt::skip]
        assert_eq!(*patched, [
            9, 0, 0, 0,
            1, 0, 0, 0, 7, 0, 0, 0,
            5, 0, 0, 0,
        ]);
        assert_eq!(
            diff(&object, &patched).collect::<Vec<_>>(),
            [(12, &[5][..]), (14, &[0][..])]
        );
        Ok(())
    }

    #[test]
    fn a_value_too_wide_for_the_source_field_fails() -> Result<()> {
        let (source, target) = clips();
        let compat = Compat::plan(&source, &target)?.expect("the clip needs fixing");
        let object = [0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 1, 0];
        let error = compat
            .apply(&mut Cow::Borrowed(&object[..]), &v2020())
            .expect_err("65536 curves don't fit a UInt16");
        assert_eq!(
            format!("{error:#}"),
            "curveCount: Int(65536) is out of range for a U16"
        );
        Ok(())
    }

    #[test]
    fn a_renamed_field_is_unfixable() -> Result<()> {
        let source = node(
            "Rigidbody2D",
            "Base",
            vec![leaf("float", "m_LinearDamping")],
        );
        let target = node("Rigidbody2D", "Base", vec![leaf("float", "m_LinearDrag")]);
        let compat = Compat::plan(&source, &target)?.expect("the rename is reported");
        assert_eq!(
            compat.unfixable(),
            ["Rigidbody2D.m_LinearDrag: renamed from m_LinearDamping"]
        );
        Ok(())
    }

    #[test]
    fn matching_trees_need_nothing() -> Result<()> {
        let (_, target) = clips();
        assert!(Compat::plan(&target, &target)?.is_none());
        Ok(())
    }
}
