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
    pub fn merge<'a>(
        sources: impl IntoIterator<Item = &'a TypeTreeNode>,
    ) -> Result<Option<MergedTypeTree>, TypeConflictError> {
        let sources: Vec<(usize, &TypeTreeNode)> = sources.into_iter().enumerate().collect();
        if sources.is_empty() {
            return Ok(None);
        }
        merge_nodes(&sources).map(Some)
    }

    /// Build a [`MergedTypeTree`] from a single source (index `0`).
    pub fn from_single(node: &TypeTreeNode) -> MergedTypeTree {
        merge_nodes(&[(0, node)]).expect("a single source cannot conflict with itself")
    }
}

fn merge_nodes(sources: &[(usize, &TypeTreeNode)]) -> Result<MergedTypeTree, TypeConflictError> {
    let repr = sources[0].1;
    if sources.iter().any(|(_, n)| n.m_Type != repr.m_Type) {
        return Err(TypeConflictError {
            field: repr.m_Name.clone(),
            types: sources.iter().map(|(_, n)| n.m_Type.clone()).collect(),
        });
    }
    let present_in = sources.iter().map(|&(id, _)| id).collect();

    let mut order: Vec<&str> = Vec::new();
    for (_, source) in sources {
        for child in &source.children {
            if !order.contains(&child.m_Name.as_str()) {
                order.push(&child.m_Name);
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
        m_Type: repr.m_Type.clone(),
        m_Name: repr.m_Name.clone(),
        present_in,
        children,
    })
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
            MergedTypeTree::merge(std::iter::empty::<&TypeTreeNode>())
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
        let m = MergedTypeTree::merge([&a, &b]).unwrap().unwrap();

        assert_eq!(m.present_in, vec![0, 1]);
        assert_eq!(child(&m, "shared").present_in, vec![0, 1]);
        assert_eq!(child(&m, "onlyA").present_in, vec![0]);
    }

    #[test]
    fn disjoint_fields_keep_first_seen_order() {
        let a = node("T", "Base", vec![leaf("int", "x")]);
        let b = node("T", "Base", vec![leaf("int", "y")]);
        let m = MergedTypeTree::merge([&a, &b]).unwrap().unwrap();

        let order: Vec<_> = m
            .children
            .iter()
            .map(|c| (c.m_Name.as_str(), c.present_in.clone()))
            .collect();
        assert_eq!(order, vec![("x", vec![0]), ("y", vec![1])]);
    }

    #[test]
    fn grandchild_of_subset_parent_stays_full() {
        let a = node(
            "Fsm",
            "Base",
            vec![node("Sub", "onlyA", vec![leaf("int", "g")])],
        );
        let b = node("Fsm", "Base", vec![]);
        let m = MergedTypeTree::merge([&a, &b]).unwrap().unwrap();

        let sub = child(&m, "onlyA");
        assert_eq!(sub.present_in, vec![0]);
        // the grandchild is present in every source that has its parent, so it is not a subset
        assert_eq!(sub.children[0].present_in, vec![0]);
    }

    #[test]
    fn differing_type_for_same_field_is_an_error() {
        let a = node("Fsm", "Base", vec![leaf("int", "x")]);
        let b = node("Fsm", "Base", vec![leaf("float", "x")]);
        let err = MergedTypeTree::merge([&a, &b]).unwrap_err();

        assert_eq!(err.field, "x");
        assert_eq!(err.types, vec!["int".to_string(), "float".to_string()]);
    }
}
