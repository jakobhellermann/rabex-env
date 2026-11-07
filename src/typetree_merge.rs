//! Merge several [`TypeTreeNode`]s describing the same type across versions.
//!
//! The result, [`MergedTypeTree`], mirrors the shape of a typetree but records for every
//! node *which* sources contain it.

use rabex::typetree::TypeTreeNode;

/// A typetree merged across one or more sources.
#[derive(Debug, Clone)]
#[allow(non_snake_case)]
pub struct MergedTypeTree {
    /// Unity type name
    pub m_Type: String,
    /// Field name
    pub m_Name: String,
    /// Indices (into the sources passed to [`merge`](Self::merge)) of the sources that
    /// contain this node, in source order. A child whose `present_in` is shorter than its
    /// parent's occurs in only a subset of the sources that have the parent.
    pub present_in: Vec<usize>,
    /// Children, unified by name across sources, in first-seen order.
    pub children: Vec<MergedTypeTree>,
}

/// Typetrees could not be cleanly merged.
///
/// Two sources give a node with the same field name a different Unity type, so they can't be
/// merged into one definition.
#[derive(Debug, Clone)]
pub struct TypeConflictError {
    /// field name (`m_Name`) of the conflicting node
    pub field: String,
    /// the differing `m_Type` values, in source order
    pub types: Vec<String>,
}

impl std::fmt::Display for TypeConflictError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "cannot merge field {:?}: differing types {}",
            self.field,
            self.types.join(" vs ")
        )
    }
}
impl std::error::Error for TypeConflictError {}

impl MergedTypeTree {
    /// Merge `sources`, all of which must describe the same node (root type). Each node's
    /// position in `sources` is recorded in [`present_in`](Self::present_in).
    ///
    /// Returns `Ok(None)` if `sources` is empty, and [`Err`] if two sources give a node with
    /// the same field name a different type ([`TypeConflictError`]).
    ///
    /// A child's presence is evaluated relative to the sources that contain its parent, so a
    /// grandchild present in every source that has its (subset-only) parent stays "full"
    /// relative to that parent.
    /// The `usize` of each source identifies it in [`present_in`](MergedTypeTree::present_in),
    /// so callers can skip sources without shifting the ids.
    pub fn merge<'a>(
        sources: impl IntoIterator<Item = (usize, &'a TypeTreeNode)>,
    ) -> Result<Option<MergedTypeTree>, TypeConflictError> {
        let sources: Vec<(usize, &TypeTreeNode)> = sources.into_iter().collect();
        if sources.is_empty() {
            return Ok(None);
        }
        merge_nodes(&sources).map(Some)
    }

    /// Build a [`MergedTypeTree`] from a single source (index `0`).
    pub fn from_single(node: &TypeTreeNode) -> MergedTypeTree {
        merge_nodes(&[(0, node)]).expect("a single source cannot conflict with itself")
    }

    /// Union two merged trees of the same type, like merging sources that each contain part
    /// of the tree. Returns the union and whether it grew, so callers can rediscover
    /// dependencies only when something changed.
    pub fn union(self, other: Self) -> Result<(Self, bool), TypeConflictError> {
        let mut changed = false;
        let m_type = if self.m_Type == other.m_Type {
            self.m_Type.clone()
        } else {
            unified_integer_type(&[self.m_Type.as_str(), other.m_Type.as_str()]).ok_or_else(
                || TypeConflictError {
                    field: self.m_Name.clone(),
                    types: vec![self.m_Type.clone(), other.m_Type.clone()],
                },
            )?
        };
        let mut present_in = self.present_in;
        for id in other.present_in {
            if !present_in.contains(&id) {
                present_in.push(id);
                changed = true;
            }
        }
        let mut children = self.children;
        let names: Vec<String> = other.children.iter().map(|c| c.m_Name.clone()).collect();
        for (k, child) in other.children.into_iter().enumerate() {
            if let Some(existing) = children.iter_mut().find(|c| c.m_Name == child.m_Name) {
                let (merged, child_changed) =
                    std::mem::replace(existing, dummy(&child)).union(child)?;
                *existing = merged;
                changed |= child_changed;
            } else {
                let order: Vec<&str> = children.iter().map(|c| c.m_Name.as_str()).collect();
                let source: Vec<&str> = names.iter().map(String::as_str).collect();
                children.insert(place(&order, &source, k), child);
                changed = true;
            }
        }
        Ok((
            MergedTypeTree {
                m_Type: m_type,
                m_Name: self.m_Name,
                present_in,
                children,
            },
            changed,
        ))
    }
}

/// Where field `k` of `source` goes in `order`, which lacks it: right after the nearest field
/// before it in its source that `order` has, else right before the nearest one after it,
/// else last — where the trees that carry it put it, since the bytes are read in tree order.
fn place(order: &[&str], source: &[&str], k: usize) -> usize {
    let at = |name: &&str| order.iter().position(|o| o == name);
    if let Some(before) = source[..k].iter().rev().find_map(at) {
        return before + 1;
    }
    source[k + 1..].iter().find_map(at).unwrap_or(order.len())
}

fn dummy(like: &MergedTypeTree) -> MergedTypeTree {
    MergedTypeTree {
        m_Type: like.m_Type.clone(),
        m_Name: like.m_Name.clone(),
        present_in: Vec::new(),
        children: Vec::new(),
    }
}

fn merge_nodes(sources: &[(usize, &TypeTreeNode)]) -> Result<MergedTypeTree, TypeConflictError> {
    let repr = sources[0].1;
    let mut m_type = repr.m_Type.clone();
    if sources.iter().any(|(_, n)| n.m_Type != m_type) {
        // integer types of different width unify to the widest, like int and SInt64 across
        // Unity versions; anything else is a real conflict
        m_type = unified_integer_type(
            &sources
                .iter()
                .map(|(_, n)| n.m_Type.as_str())
                .collect::<Vec<_>>(),
        )
        .ok_or_else(|| TypeConflictError {
            field: repr.m_Name.clone(),
            types: sources.iter().map(|(_, n)| n.m_Type.clone()).collect(),
        })?;
    }
    let present_in = sources.iter().map(|&(id, _)| id).collect();

    let mut order: Vec<&str> = Vec::new();
    for (_, source) in sources {
        let names: Vec<&str> = source.children.iter().map(|c| c.m_Name.as_str()).collect();
        for (k, name) in names.iter().enumerate() {
            if !order.contains(name) {
                order.insert(place(&order, &names, k), name);
            }
        }
    }

    let children = order
        .into_iter()
        .map(|name| {
            let variants: Vec<(usize, &TypeTreeNode)> = sources
                .iter()
                .filter_map(|&(id, source)| {
                    source
                        .children
                        .iter()
                        .find(|c| c.m_Name == name)
                        .map(|c| (id, c))
                })
                .collect();
            merge_nodes(&variants)
        })
        .collect::<Result<_, _>>()?;

    Ok(MergedTypeTree {
        m_Type: m_type,
        m_Name: repr.m_Name.clone(),
        present_in,
        children,
    })
}

/// The widest integer type when `types` are all signed or all unsigned integers of
/// different width (like `int` and `SInt64` across Unity versions), else `None`.
fn unified_integer_type(types: &[&str]) -> Option<String> {
    const SIGNED: [&str; 4] = ["SInt8", "SInt16", "int", "SInt64"];
    const UNSIGNED: [&str; 4] = ["UInt8", "UInt16", "UInt32", "UInt64"];
    let (group, order): (&[&str; 4], &[&str; 4]) = if types.iter().all(|t| SIGNED.contains(t)) {
        (&SIGNED, &SIGNED)
    } else if types.iter().all(|t| UNSIGNED.contains(t)) {
        (&UNSIGNED, &UNSIGNED)
    } else {
        return None;
    };
    let _ = group;
    types
        .iter()
        .map(|t| order.iter().position(|o| o == t).unwrap())
        .max()
        .map(|i| order[i].to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(ty: &str, name: &str, children: Vec<TypeTreeNode>) -> TypeTreeNode {
        TypeTreeNode {
            m_Type: ty.into(),
            m_Name: name.into(),
            children,
            ..Default::default()
        }
    }
    fn leaf(ty: &str, name: &str) -> TypeTreeNode {
        node(ty, name, vec![])
    }
    fn child<'a>(m: &'a MergedTypeTree, name: &str) -> &'a MergedTypeTree {
        m.children.iter().find(|c| c.m_Name == name).unwrap()
    }

    #[test]
    fn empty_sources_is_none() {
        assert!(
            MergedTypeTree::merge(std::iter::empty::<(usize, &TypeTreeNode)>())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn field_in_only_one_source_is_a_subset() {
        let a = node(
            "Fsm",
            "Base",
            vec![leaf("int", "shared"), leaf("int", "onlyA")],
        );
        let b = node("Fsm", "Base", vec![leaf("int", "shared")]);
        let m = MergedTypeTree::merge([(0, &a), (1, &b)]).unwrap().unwrap();

        assert_eq!(m.present_in, vec![0, 1]);
        assert_eq!(child(&m, "shared").present_in, vec![0, 1]);
        assert_eq!(child(&m, "onlyA").present_in, vec![0]);
    }

    #[test]
    fn disjoint_fields_keep_first_seen_order() {
        let a = node("T", "Base", vec![leaf("int", "x")]);
        let b = node("T", "Base", vec![leaf("int", "y")]);
        let m = MergedTypeTree::merge([(0, &a), (1, &b)]).unwrap().unwrap();

        let order: Vec<_> = m
            .children
            .iter()
            .map(|c| (c.m_Name.as_str(), c.present_in.clone()))
            .collect();
        assert_eq!(order, vec![("x", vec![0]), ("y", vec![1])]);
    }

    /// A field the first source lacks goes where the source that has it puts it, not after
    /// the first source's fields.
    #[test]
    fn a_field_of_later_sources_keeps_its_place() {
        let first = node("S", "Base", vec![leaf("int", "a"), leaf("int", "c")]);
        let second = node(
            "S",
            "Base",
            vec![leaf("int", "a"), leaf("int", "b"), leaf("int", "c")],
        );
        let names = |m: &MergedTypeTree| {
            m.children
                .iter()
                .map(|c| c.m_Name.clone())
                .collect::<Vec<_>>()
        };
        let merged = MergedTypeTree::merge([(0, &first), (1, &second)])
            .unwrap()
            .unwrap();
        assert_eq!(names(&merged), ["a", "b", "c"]);
        let (unioned, _) = MergedTypeTree::from_single(&first)
            .union(MergedTypeTree::from_single(&second))
            .unwrap();
        assert_eq!(names(&unioned), ["a", "b", "c"]);
    }

    #[test]
    fn grandchild_of_subset_parent_stays_full() {
        let a = node(
            "Fsm",
            "Base",
            vec![node("Sub", "onlyA", vec![leaf("int", "g")])],
        );
        let b = node("Fsm", "Base", vec![]);
        let m = MergedTypeTree::merge([(0, &a), (1, &b)]).unwrap().unwrap();

        let sub = child(&m, "onlyA");
        assert_eq!(sub.present_in, vec![0]);
        // the grandchild is present in every source that has its parent, so it is not a subset
        assert_eq!(sub.children[0].present_in, vec![0]);
    }

    #[test]
    fn differing_type_for_same_field_is_an_error() {
        let a = node("Fsm", "Base", vec![leaf("int", "x")]);
        let b = node("Fsm", "Base", vec![leaf("float", "x")]);
        let err = MergedTypeTree::merge([(0, &a), (1, &b)]).unwrap_err();

        assert_eq!(err.field, "x");
        assert_eq!(err.types, vec!["int".to_string(), "float".to_string()]);
    }
    #[test]
    fn occurrences_union_their_fields() {
        // one occurrence carries the flat fields of the older version, another the
        // m_IsPartialCB the newer one added
        let flat = merge_nodes(&[(0, &node("Scope", "cb", vec![leaf("int", "m_Size")]))]);
        let modern = merge_nodes(&[(
            0,
            &node(
                "Scope",
                "cb",
                vec![leaf("int", "m_Size"), leaf("bool", "m_IsPartialCB")],
            ),
        )]);
        let (unioned, changed) = flat.unwrap().union(modern.unwrap()).unwrap();
        assert!(changed);
        assert_eq!(unioned.present_in, vec![0]);
        assert_eq!(
            unioned
                .children
                .iter()
                .map(|c| c.m_Name.as_str())
                .collect::<Vec<_>>(),
            ["m_Size", "m_IsPartialCB"]
        );
        assert_eq!(unioned.children[1].present_in, vec![0]);

        // unioning two identical trees changes nothing
        let modern = merge_nodes(&[(
            0,
            &node(
                "Scope",
                "cb",
                vec![leaf("int", "m_Size"), leaf("bool", "m_IsPartialCB")],
            ),
        )])
        .unwrap();
        let (same, changed) = modern.clone().union(modern).unwrap();
        assert!(!changed);
        assert_eq!(same.children.len(), 2);
    }
}
