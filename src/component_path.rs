//! Component references like `Root/Child:2@SpriteRenderer:1`.
//!
//! A reference points at a GameObject by its Transform-hierarchy path and, optionally, one of its
//! components. Neither path segments nor components are unique, so each may carry a `:<index>` to
//! disambiguate among equally matching siblings/components (0-based). The structural characters
//! `/`, `@`, `:` and `\` can be escaped with a backslash to use them literally in a name.
//!
//! Grammar:
//! ```text
//! path     := segment ('/' segment)* ('@' selector)?
//! segment  := name (':' index)?
//! selector := name (':' index)?
//! ```

use std::fmt;

use rabex::objects::ClassId;
use rabex::objects::pptr::PathId;

/// A reference to a GameObject (by hierarchy path) and optionally a component on it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ComponentPath {
    /// Hierarchy path, root first; always at least one segment.
    pub segments: Vec<PathSegment>,
    /// Component selector (`@Type[:index]`), if any.
    pub component: Option<Component>,
}

/// A GameObject name in the hierarchy path, with an optional index disambiguating equal-named
/// siblings.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PathSegment {
    pub name: String,
    pub index: Option<usize>,
}

/// Identifies a component by type: a built-in Unity class, or a MonoBehaviour by its script class
/// name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ComponentId {
    Class(ClassId),
    Script(String),
}

impl ComponentId {
    /// The textual type label: the class name, or the script's class name.
    pub fn label(&self) -> String {
        match self {
            ComponentId::Class(class) => class
                .name()
                .map(str::to_owned)
                .unwrap_or_else(|| format!("{class:?}")),
            ComponentId::Script(name) => name.clone(),
        }
    }
}

/// A component selector, with an optional index disambiguating equal-typed components on the same
/// GameObject.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Component {
    pub id: ComponentId,
    pub index: Option<usize>,
}

/// How an object is addressed: by raw path id, or by a hierarchy/component path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObjectRef {
    PathId(PathId),
    Path(ComponentPath),
}

/// Parse an [`ObjectRef`]: a bare integer is a path id, anything else a [`ComponentPath`].
pub fn parse_object_ref(input: &str) -> Result<ObjectRef, String> {
    match input.parse::<i64>() {
        Ok(path_id) => Ok(ObjectRef::PathId(path_id)),
        Err(_) => Ok(ObjectRef::Path(parse(input)?)),
    }
}

/// `Display` is the inverse of [`parse`]: it escapes structural characters so the output round-trips
/// back through `parse`.
impl fmt::Display for ComponentPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, segment) in self.segments.iter().enumerate() {
            if i > 0 {
                f.write_str("/")?;
            }
            write!(f, "{segment}")?;
        }
        if let Some(component) = &self.component {
            write!(f, "@{component}")?;
        }
        Ok(())
    }
}

/// Serializes as its [`Display`] string (e.g. `"Root/Child@PlayMakerFSM:1"`).
impl serde::Serialize for ComponentPath {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

/// Deserializes from the [`Display`] string via [`parse`], round-tripping with [`Serialize`].
impl<'de> serde::Deserialize<'de> for ComponentPath {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
        parse(&s).map_err(serde::de::Error::custom)
    }
}

impl fmt::Display for PathSegment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&escape(&self.name))?;
        if let Some(index) = self.index {
            write!(f, ":{index}")?;
        }
        Ok(())
    }
}

impl fmt::Display for Component {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&escape(&self.id.label()))?;
        if let Some(index) = self.index {
            write!(f, ":{index}")?;
        }
        Ok(())
    }
}

/// Backslash-escape the structural characters so a name round-trips.
fn escape(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if matches!(c, '\\' | '/' | '@' | ':') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Parse a [`ComponentPath`].
///
/// The component selector is always parsed as [`ComponentId::Script`]; turning a built-in class
/// name back into [`ComponentId::Class`] needs a name→[`ClassId`] lookup that lives elsewhere.
pub fn parse(input: &str) -> Result<ComponentPath, String> {
    // The component selector is everything after the first unescaped '@'.
    let at = split_keep_escapes(input, '@');
    let (path_part, component) = match at.as_slice() {
        [path] => (path.as_str(), None),
        [path, selector] => {
            let (name, index) = parse_name_index(selector, "component")?;
            let component = Component {
                id: ComponentId::Script(name),
                index,
            };
            (path.as_str(), Some(component))
        }
        _ => return Err("at most one '@' component selector is allowed".to_owned()),
    };

    let segments = split_keep_escapes(path_part, '/')
        .iter()
        .map(|seg| {
            parse_name_index(seg, "path segment").map(|(name, index)| PathSegment { name, index })
        })
        .collect::<Result<Vec<_>, _>>()?;

    if segments.iter().any(|s| s.name.is_empty()) {
        return Err("empty path segment".to_owned());
    }
    Ok(ComponentPath {
        segments,
        component,
    })
}

fn parse_name_index(raw: &str, what: &str) -> Result<(String, Option<usize>), String> {
    let parts = split_keep_escapes(raw, ':');
    match parts.as_slice() {
        [name] => Ok((unescape(name), None)),
        [name, index] => {
            let index = index
                .parse::<usize>()
                .map_err(|_| format!("invalid index ':{index}' (expected a number)"))?;
            Ok((unescape(name), Some(index)))
        }
        _ => Err(format!("at most one ':index' is allowed per {what}")),
    }
}

/// Split on unescaped `delim`, leaving any other `\x` escapes intact in the pieces (so a later split
/// on a different delimiter still sees them escaped).
fn split_keep_escapes(s: &str, delim: char) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            cur.push('\\');
            if let Some(next) = chars.next() {
                cur.push(next);
            }
        } else if c == delim {
            parts.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    parts.push(cur);
    parts
}

/// Remove escaping backslashes, yielding the literal name.
fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(name: &str, index: Option<usize>) -> PathSegment {
        PathSegment {
            name: name.to_owned(),
            index,
        }
    }

    fn script(name: &str, index: Option<usize>) -> Component {
        Component {
            id: ComponentId::Script(name.to_owned()),
            index,
        }
    }

    #[test]
    fn plain_path_no_component() {
        assert_eq!(
            parse("Root/Child").unwrap(),
            ComponentPath {
                segments: vec![seg("Root", None), seg("Child", None)],
                component: None,
            }
        );
    }

    #[test]
    fn path_with_component() {
        assert_eq!(
            parse("Object/Path@SpriteRenderer").unwrap(),
            ComponentPath {
                segments: vec![seg("Object", None), seg("Path", None)],
                component: Some(script("SpriteRenderer", None)),
            }
        );
    }

    #[test]
    fn indices_on_segments_and_component() {
        assert_eq!(
            parse("Path/To:3/Component@FsmStateMachine:6").unwrap(),
            ComponentPath {
                segments: vec![
                    seg("Path", None),
                    seg("To", Some(3)),
                    seg("Component", None)
                ],
                component: Some(script("FsmStateMachine", Some(6))),
            }
        );
    }

    #[test]
    fn escaped_colon_in_name() {
        assert_eq!(
            parse(r"weird\:name@Comp").unwrap(),
            ComponentPath {
                segments: vec![seg("weird:name", None)],
                component: Some(script("Comp", None)),
            }
        );
    }

    #[test]
    fn escaped_slash_and_at_in_name() {
        assert_eq!(
            parse(r"a\/b\@c").unwrap(),
            ComponentPath {
                segments: vec![seg("a/b@c", None)],
                component: None,
            }
        );
    }

    #[test]
    fn display_roundtrips_through_parse() {
        for s in [
            "Root/Child",
            "Object/Path@SpriteRenderer",
            "Path/To:3@FsmStateMachine:6",
            r"weird\:name@Comp",
            r"a\/b\@c",
        ] {
            assert_eq!(parse(s).unwrap().to_string(), s);
        }
    }

    #[test]
    fn class_component_displays_its_class_name() {
        let path = ComponentPath {
            segments: vec![seg("Player", None)],
            component: Some(Component {
                id: ComponentId::Class(ClassId::Transform),
                index: Some(1),
            }),
        };
        assert_eq!(path.to_string(), "Player@Transform:1");
    }

    #[test]
    fn serde_roundtrips_as_string() {
        let path = ComponentPath {
            segments: vec![seg("Root", None), seg("Dup", Some(1))],
            component: Some(script("PlayMakerFSM", None)),
        };
        let json = serde_json::to_string(&path).unwrap();
        assert_eq!(json, "\"Root/Dup:1@PlayMakerFSM\"");
        assert_eq!(serde_json::from_str::<ComponentPath>(&json).unwrap(), path);
    }

    #[test]
    fn errors() {
        assert!(parse("").is_err());
        assert!(parse("a//b").is_err());
        assert!(parse("a@b@c").is_err());
        assert!(parse("a:notanumber").is_err());
        assert!(parse("a:1:2").is_err());
    }
}
