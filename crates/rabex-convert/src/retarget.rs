//! Retargeting serialized files across Unity versions.
//!
//! Engine classes whose type tree changed between the source and the target version are
//! re-encoded: the object decodes against its source tree and serializes against the target
//! tree. Fields the target lost drop knowingly and land in the report; fields the target wants
//! that the data lacks fail loudly in the serializer's coverage check. MonoBehaviour objects
//! carry over — their layout is script-defined, not engine-version-defined.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};

use crate::{material, shader, texture};
use anyhow::{Context, Result, bail};
use rabex::files::SerializedFile;
use rabex::files::serializedfile::{CommonOffsetMap, ObjectInfo};
use rabex::objects::{ClassId, PPtr};
use rabex::typetree::{TypeTreeNode, TypeTreeProvider};
use rabex::{UnityVersion, serde_typetree};

use crate::bundle::rewrite_bundle;
use crate::layout::Layout;
use crate::rewriter::{Output, SerializedFileRewriter, Source};
use crate::transcode::Transcoder;
use crate::value::layout_equal;

/// What a retarget did to one serialized file.
#[derive(Debug, Default, PartialEq)]
pub struct Report {
    /// Objects decoded and re-encoded because their class's type tree changed.
    pub reencoded: usize,
    /// Objects carried over unchanged.
    pub carried: usize,
    /// Fields of the source version the target tree has no slot for, by field path with the
    /// number of objects that dropped them.
    pub dropped: BTreeMap<String, usize>,
    /// Objects of classes the target version does not have at all, by class with the
    /// number removed.
    pub removed: BTreeMap<String, usize>,
    /// Serialized script fields renamed to what the target's class reads,
    /// `class.from -> to`, with the number of type entries.
    pub renamed_fields: BTreeMap<String, usize>,
    /// What rules wrote that the target cannot say exactly, by message, with the number of
    /// objects.
    pub warnings: BTreeMap<String, usize>,
}

/// A script class and the `(from, to)` renames of its serialized fields that the
/// target's class reads under another name.
pub struct ScriptRenames {
    /// `Namespace.Class`.
    pub class: String,
    pub fields: Vec<(String, String)>,
}

/// What a retarget needs to know about scripts, which lives in other files and in
/// assemblies rather than in the file being retargeted.
pub trait Scripts: Sync {
    /// The renames for the script `script` names, a reference out of `file`, whose bytes
    /// are `data`.
    fn renames(&self, file: &SerializedFile, data: &[u8], script: PPtr) -> Result<ScriptRenames>;
}

impl Report {
    /// Add `other`'s counts to these.
    pub fn merge(&mut self, other: Report) {
        self.reencoded += other.reencoded;
        self.carried += other.carried;
        for (map, from) in [
            (&mut self.dropped, other.dropped),
            (&mut self.removed, other.removed),
            (&mut self.renamed_fields, other.renamed_fields),
            (&mut self.warnings, other.warnings),
        ] {
            for (k, v) in from {
                *map.entry(k).or_default() += v;
            }
        }
    }

    /// Whether the retarget changed nothing.
    pub fn unchanged(&self) -> bool {
        self.reencoded == 0
    }
}

/// Retarget serialized files to `typetrees`' trees of the target version.
pub struct Retarget<'a, P: TypeTreeProvider> {
    pub to: UnityVersion,
    pub typetrees: &'a P,
    /// The chunk size the rebuilt shader platform blobs pack their entries into, the
    /// build's `m_DefaultShaderChunkSizeInMB`.
    pub shader_chunk_size: usize,
    /// The target version's common string table, `rabex::files::serializedfile::
    /// build_common_offset_map` for a [`rabex::tpk::TpkTypeTreeBlob`]: embedded trees
    /// reference these strings by offset the way the target player's own files do, and
    /// strings the table lacks write inline.
    pub common_strings: HashMap<&'a str, u32>,
    /// The version to read a bundle as when its header carries none, as Addressables
    /// bundles do.
    pub source_version: Option<UnityVersion>,
    /// Scripts' identities and field renames; without, script data carries over as is.
    pub scripts: Option<&'a dyn Scripts>,
}

/// Rename the serialized fields of every script type entry objects use to what the
/// target's class reads. Runs before type entries are removed, while their indices are
/// still the source's.
fn rename_script_fields<P: TypeTreeProvider>(
    rewriter: &mut SerializedFileRewriter<'_, P>,
    scripts: &dyn Scripts,
    report: &mut Report,
) -> Result<()> {
    let (file, data) = (rewriter.source.file(), rewriter.source.data());
    let mut planned = Vec::new();
    let mut seen = HashSet::new();
    // a type entry's tree is what its objects read: ask through an object, which also
    // covers formats that keep the script index on the object
    for info in file.objects() {
        let Ok(index) = usize::try_from(info.m_TypeID) else {
            continue;
        };
        if !seen.insert(index) {
            continue;
        }
        let Some(script) = file.script_type(info) else {
            continue;
        };
        let renames = scripts.renames(file, data, script)?;
        if !renames.fields.is_empty() {
            planned.push((index, renames));
        }
    }
    for (index, renames) in planned {
        for (from, to) in rewriter.rename_type_fields(index, &renames.fields)? {
            *report
                .renamed_fields
                .entry(format!("{}.{from} -> {to}", renames.class))
                .or_default() += 1;
        }
    }
    Ok(())
}

/// Whether the object rule of `source`'s class converts between its form and `target`'s:
/// the fields each rework left behind, on either side of it. Within one form the plan
/// carries the objects like any other class's.
fn object_rule(source: &TypeTreeNode, target: &TypeTreeNode) -> bool {
    use crate::value::has_field;
    let differ = |tree: fn(&TypeTreeNode) -> bool| tree(source) != tree(target);
    match source.m_Type.as_str() {
        // the keyword names of Unity 2021.2's shader rework, in the parsed form
        "Shader" => differ(|tree| {
            tree.children
                .iter()
                .find(|child| child.m_Name == "m_ParsedForm")
                .is_some_and(|form| has_field(form, "m_KeywordNames"))
        }),
        // the keyword vectors of the same rework
        "Material" => differ(|tree| has_field(tree, "m_ValidKeywords")),
        // the mipmap limit flag of Unity 2022.2
        "Texture2D" => differ(|tree| has_field(tree, "m_IgnoreMipmapLimit")),
        _ => false,
    }
}

/// Read one object of `T` and encode what `convert` makes of it against `target`.
fn convert<P: TypeTreeProvider, T, F>(
    source: &Source<'_, P>,
    info: &ObjectInfo,
    target: &TypeTreeNode,
    convert: F,
) -> Result<Vec<u8>>
where
    T: serde::de::DeserializeOwned + serde::Serialize,
    F: FnOnce(T) -> Result<T>,
{
    let object: T = source.read(info)?;
    let converted = convert(object)?;
    Ok(serde_typetree::to_vec::<_, byteorder::LittleEndian>(
        &converted, target,
    )?)
}

/// How the objects of one type entry are re-encoded: the plans from its tree, and from
/// the tree a build extends it to, each made when the first object needs it.
struct Encoding<'t> {
    class: &'t str,
    source: &'t TypeTreeNode,
    build: Option<&'t TypeTreeNode>,
    target: &'t TypeTreeNode,
    /// The layouts of the source's tree and the build's, which tell an object's form.
    forms: Option<[Layout; 2]>,
    /// Whether the class's object rule converts between the two trees' forms.
    object_rule: bool,
    plans: [Option<(Transcoder<'t>, Vec<usize>)>; 2],
}

impl<'t> Encoding<'t> {
    fn new(
        source: &'t TypeTreeNode,
        build: Option<&'t TypeTreeNode>,
        target: &'t TypeTreeNode,
    ) -> Self {
        Self {
            class: &source.m_Type,
            source,
            build,
            target,
            forms: build.map(|build| [Layout::of(source), Layout::of(build)]),
            object_rule: object_rule(source, target),
            plans: [None, None],
        }
    }

    /// The plan for `object`, with its dropped field counts: from the tree that reads
    /// it. A build writes fields no tree names, and which form the bytes are in shows
    /// only in which tree reads them exactly.
    fn plan_for(&mut self, object: &[u8]) -> Result<&mut (Transcoder<'t>, Vec<usize>)> {
        let which = match &self.forms {
            None => 0,
            Some([plain, build]) => {
                let reads = |layout| {
                    let mut reader = crate::layout::Reader::new(object);
                    reader.skip(layout).is_ok() && reader.is_at_end()
                };
                match (reads(plain), reads(build)) {
                    (true, false) => 0,
                    (false, true) => 1,
                    (true, true) => {
                        bail!("reads exactly both with and without the fields a build adds")
                    }
                    (false, false) => bail!(
                        "the tree does not account for the object's {} bytes",
                        object.len()
                    ),
                }
            }
        };
        let tree = [Some(self.source), self.build][which].expect("chosen above");
        if self.plans[which].is_none() {
            let transcoder = Transcoder::new(tree, self.target, self.class)?;
            let counts = vec![0; transcoder.dropped().len()];
            self.plans[which] = Some((transcoder, counts));
        }
        Ok(self.plans[which].as_mut().expect("made above"))
    }

    /// Count the fields the plans dropped into `report`.
    fn report(self, report: &mut Report) {
        for (transcoder, counts) in self.plans.into_iter().flatten() {
            for (path, n) in transcoder.dropped().iter().zip(counts) {
                if n > 0 {
                    *report.dropped.entry(path.clone()).or_default() += n;
                }
            }
        }
    }
}

/// What becomes of the objects of one type entry.
enum Plan<'t> {
    /// They read the same in the target.
    Carry,
    /// The target has no such class.
    Remove(Vec<ObjectInfo>),
    /// They are re-encoded from the source's tree against the target's.
    Reencode(Box<Reencode<'t>>),
}

struct Reencode<'t> {
    source: Cow<'t, TypeTreeNode>,
    /// The source's tree as a build extends it, for classes a build writes more of.
    build: Option<TypeTreeNode>,
    target: Cow<'t, TypeTreeNode>,
    objects: Vec<ObjectInfo>,
}

impl<P: TypeTreeProvider> Retarget<'_, P> {
    /// Retarget the file of `rewriter` in place.
    pub fn apply(&self, rewriter: &mut SerializedFileRewriter<'_, P>) -> Result<Report> {
        let mut report = Report::default();
        if let Some(scripts) = self.scripts {
            rename_script_fields(rewriter, scripts, &mut report)?;
        }
        // the file's version string tells the target player what it parses; even
        // files whose objects all carry over get it
        rewriter.set_unity_version(self.to.clone());

        // what the plans read stays borrowed from the source while the output is written
        let SerializedFileRewriter { source, output } = rewriter;
        // what happens to an object depends only on its type entry's tree and the
        // target's for its class, so it is decided once per type entry
        let mut plans: BTreeMap<i32, Plan> = BTreeMap::new();
        for info in source.objects() {
            let plan = match plans.entry(info.m_TypeID) {
                std::collections::btree_map::Entry::Occupied(plan) => plan.into_mut(),
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(self.plan(source, info)?)
                }
            };
            match plan {
                Plan::Carry => report.carried += 1,
                Plan::Remove(objects) => objects.push(info.clone()),
                Plan::Reencode(reencode) => reencode.objects.push(info.clone()),
            }
        }
        for (ty, plan) in plans {
            match plan {
                Plan::Carry => {}
                Plan::Remove(objects) => {
                    for info in objects {
                        output.remove(&info)?;
                        *report
                            .removed
                            .entry(format!("{:?}", info.m_ClassID))
                            .or_default() += 1;
                    }
                }
                Plan::Reencode(reencode) => {
                    let Reencode {
                        source: tree,
                        build,
                        target,
                        objects,
                    } = *reencode;
                    let mut encoding = Encoding::new(&tree, build.as_ref(), &target);
                    for info in &objects {
                        self.reencode(source, output, info, &mut encoding, &mut report)?;
                    }
                    encoding.report(&mut report);
                    output.retype(usize::try_from(ty)?, target.into_owned());
                }
            }
        }
        Ok(report)
    }

    /// What becomes of the objects of `info`'s type entry.
    fn plan<'t>(&'t self, source: &'t Source<'_, P>, info: &ObjectInfo) -> Result<Plan<'t>> {
        if info.m_ClassID == ClassId::MonoBehaviour {
            return Ok(Plan::Carry);
        }
        let Some(target) = self.typetrees.get_typetree_node(info.m_ClassID, &self.to) else {
            // A class the target engine does not have: no player of it can read the
            // object, so it goes, and the report says so.
            // TODO: whatever references the object reads as a null reference in the
            // target. Unproven that nothing that matters does: the classes met so far
            // are managers (Unity 6's MultiplayerManager), which the engine looks up
            // by class, not by reference. A PPtr scan of the file would settle it.
            return Ok(Plan::Remove(Vec::new()));
        };
        let tree = source.tree_of(info)?;
        // a class a build writes more of than its tree names is written as the build
        // writes it: the player reads those fields
        let target = match crate::fields::build_tree(&target) {
            Some(build) => Cow::Owned(build),
            None => target,
        };
        if layout_equal(&tree, &target) {
            return Ok(Plan::Carry);
        }
        Ok(Plan::Reencode(Box::new(Reencode {
            build: crate::fields::build_tree(&tree),
            source: tree,
            target,
            objects: Vec::new(),
        })))
    }

    /// Re-encode one object against the target's tree, its type entry's tree now.
    fn reencode(
        &self,
        source: &Source<'_, P>,
        output: &mut Output<'_, P>,
        info: &ObjectInfo,
        encoding: &mut Encoding,
        report: &mut Report,
    ) -> Result<()> {
        let context = || format!("{:?} {}", info.m_ClassID, info.m_PathID);
        let target = encoding.target;
        // classes whose objects Unity reworked as a whole carry their own typed rules,
        // for the forms across that rework; everything else runs through the plan its
        // trees and the field rules make
        let object_rule = encoding.object_rule;
        let encoded = match info.m_ClassID {
            _ if !object_rule => {
                let object = source.object_bytes(info)?;
                encoding.plan_for(object).and_then(|(transcoder, counts)| {
                    transcoder.run(object, &self.to, counts, &mut report.warnings)
                })
            }
            ClassId::Shader => convert(source, info, target, |shader| {
                shader::convert(&shader, target, self.shader_chunk_size)
            }),
            ClassId::Material => convert(source, info, target, |material| {
                material::convert(material, target, &mut report.dropped)
            }),
            ClassId::Texture2D => convert(source, info, target, |texture| {
                texture::convert(texture, target)
            }),
            _ => {
                let object = source.object_bytes(info)?;
                encoding.plan_for(object).and_then(|(transcoder, counts)| {
                    transcoder.run(object, &self.to, counts, &mut report.warnings)
                })
            }
        }
        .with_context(context)?;
        output.replace_encoded(info, encoded)?;
        report.reencoded += 1;
        Ok(())
    }

    /// Retarget one serialized file, `None` if nothing changed.
    pub fn serialized_file(&self, data: &[u8]) -> Result<Option<Vec<u8>>> {
        self.serialized_file_reported(data).map(|(data, _)| data)
    }

    /// [`Self::serialized_file`] with the [`Report`] of what happened.
    pub fn serialized_file_reported(&self, data: &[u8]) -> Result<(Option<Vec<u8>>, Report)> {
        let common: CommonOffsetMap = self.common_strings.iter().map(|(s, o)| (*s, *o)).collect();
        let mut rewriter = SerializedFileRewriter::new(data, self.typetrees, &common)?;
        let report = self.apply(&mut rewriter)?;
        Ok((rewriter.finish()?, report))
    }

    /// Retarget every serialized file of a bundle, `None` if nothing changed.
    pub fn bundle(&self, bundle: &[u8]) -> Result<Option<Vec<u8>>> {
        self.bundle_reported(bundle).map(|(data, _)| data)
    }

    /// [`Self::bundle`] with the [`Report`] of its files merged.
    pub fn bundle_reported(&self, bundle: &[u8]) -> Result<(Option<Vec<u8>>, Report)> {
        let common: CommonOffsetMap = self.common_strings.iter().map(|(s, o)| (*s, *o)).collect();
        let versions = crate::bundle::Options {
            source: self.source_version.clone(),
            written: Some(self.to.clone()),
            compression: None,
        };
        let mut total = Report::default();
        let data = rewrite_bundle(bundle, self.typetrees, &common, &versions, |_, rewriter| {
            total.merge(self.apply(rewriter)?);
            Ok(())
        })?;
        Ok((data, total))
    }
}
