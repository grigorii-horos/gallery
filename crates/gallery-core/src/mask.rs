//! Filename masks: globs (`*.{jpg,heic}`) or regexes (leading `^`).

use globset::{GlobBuilder, GlobMatcher};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

use crate::{Error, Result};

#[derive(Debug, Clone)]
enum Matcher {
    Glob(GlobMatcher),
    Regex(Regex),
}

/// Case-insensitive mask matched against a file name (not a path).
/// A string starting with `^` is a regex, anything else is a glob.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Mask {
    source: String,
    matcher: Matcher,
}

impl PartialEq for Mask {
    fn eq(&self, other: &Mask) -> bool {
        self.source == other.source
    }
}

impl Eq for Mask {}

impl Mask {
    pub fn parse(s: &str) -> Result<Mask> {
        let bad =
            |e: &dyn std::fmt::Display| Error::Config(crate::t!("mask.bad", mask = s, detail = e));
        let matcher = if s.starts_with('^') {
            let re = RegexBuilder::new(s)
                .case_insensitive(true)
                .build()
                .map_err(|e| bad(&e))?;
            Matcher::Regex(re)
        } else {
            // globset folds case for ASCII only, so lowercase both sides.
            let glob = GlobBuilder::new(&s.to_lowercase())
                .case_insensitive(true)
                .literal_separator(false)
                .build()
                .map_err(|e| bad(&e))?;
            Matcher::Glob(glob.compile_matcher())
        };
        Ok(Mask {
            source: s.to_string(),
            matcher,
        })
    }

    pub fn matches(&self, file_name: &str) -> bool {
        match &self.matcher {
            Matcher::Glob(g) => g.is_match(file_name.to_lowercase()),
            Matcher::Regex(r) => r.is_match(file_name),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.source
    }
}

impl TryFrom<String> for Mask {
    type Error = Error;
    fn try_from(s: String) -> Result<Mask> {
        Mask::parse(&s)
    }
}

impl From<Mask> for String {
    fn from(m: Mask) -> String {
        m.source
    }
}

/// Where `@name` references are looked up (the config: its own `[masks]`,
/// then the built-in ones).
pub trait MaskScope {
    fn named_mask(&self, name: &str) -> Option<&MaskExpr>;
}

/// A scope without any named masks.
impl MaskScope for () {
    fn named_mask(&self, _name: &str) -> Option<&MaskExpr> {
        None
    }
}

/// How deep `@name` references may nest while matching; `Config::validate`
/// rejects cycles, this only keeps matching finite on an unvalidated config.
pub const MAX_REF_DEPTH: usize = 16;

/// One item of a mask expression: a pattern, `@name`, or `!` + either of
/// those (an exclusion).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaskItem {
    Pattern(Mask),
    Ref(String),
    /// `!pattern` / `!@name`: a name matching it is rejected.
    Not(Box<MaskItem>),
}

impl MaskItem {
    pub fn parse(s: &str) -> Result<MaskItem> {
        if let Some(rest) = s.strip_prefix('!') {
            if rest.starts_with(char::is_whitespace) && !rest.trim().is_empty() {
                return Err(Error::Config(crate::t!("mask.bad_space", mask = s)));
            }
            if rest.trim().is_empty() {
                return Err(Error::Config(crate::t!("mask.bad_empty_not", mask = s)));
            }
            if rest.starts_with('!') {
                return Err(Error::Config(crate::t!("mask.bad_double_not", mask = s)));
            }
            return Ok(MaskItem::Not(Box::new(MaskItem::parse(rest)?)));
        }
        match s.strip_prefix('@') {
            Some("") => Err(Error::Config(crate::t!("mask.bad_ref_name"))),
            Some(name) => Ok(MaskItem::Ref(name.to_string())),
            None => Mask::parse(s).map(MaskItem::Pattern),
        }
    }

    pub fn as_string(&self) -> String {
        match self {
            MaskItem::Pattern(m) => m.as_str().to_string(),
            MaskItem::Ref(n) => format!("@{n}"),
            MaskItem::Not(i) => format!("!{}", i.as_string()),
        }
    }

    pub fn is_negated(&self) -> bool {
        matches!(self, MaskItem::Not(_))
    }

    fn ref_name(&self) -> Option<&str> {
        match self {
            MaskItem::Ref(n) => Some(n),
            MaskItem::Not(i) => i.ref_name(),
            MaskItem::Pattern(_) => None,
        }
    }

    /// Whether the (un-negated) item matches. An unresolvable reference
    /// (unknown or nested too deep) yields `unresolved`.
    fn hits(
        &self,
        file_name: &str,
        scope: &(impl MaskScope + ?Sized),
        depth: usize,
        unresolved: bool,
    ) -> bool {
        match self {
            MaskItem::Pattern(m) => m.matches(file_name),
            MaskItem::Ref(n) => match scope.named_mask(n) {
                Some(e) if depth < MAX_REF_DEPTH => e.matches_at(file_name, scope, depth + 1),
                _ => unresolved,
            },
            MaskItem::Not(i) => i.hits(file_name, scope, depth, unresolved),
        }
    }
}

fn check_item(
    item: &MaskItem,
    name: &str,
    scope: &(impl MaskScope + ?Sized),
    depth: usize,
    veto: bool,
) -> ItemCheck {
    let negated = item.is_negated();
    let inner = match item {
        MaskItem::Not(i) => i.as_ref(),
        i => i,
    };
    let unresolved = veto || negated;
    let hit = item.hits(name, scope, depth, unresolved);
    let (expanded, children) = match inner {
        MaskItem::Ref(n) if depth < MAX_REF_DEPTH => match scope.named_mask(n) {
            Some(e) => (
                e.expand(scope),
                e.0.iter()
                    .map(|c| check_item(c, name, scope, depth + 1, veto))
                    .collect(),
            ),
            None => (String::new(), Vec::new()),
        },
        _ => (String::new(), Vec::new()),
    };
    let ext = name
        .rsplit_once('.')
        .map_or(String::new(), |(_, e)| format!(".{e}"));
    let pos = children.iter().find(|c| !c.negated && c.hit);
    let neg = children.iter().find(|c| c.negated && c.hit);
    let reason = match (negated, hit) {
        (false, true) => crate::t!("mask.matched"),
        (false, false) => match (pos, neg) {
            (Some(p), Some(n)) => {
                crate::t!(
                    "mask.matched_but_excluded",
                    taker = p.item,
                    excluder = n.item
                )
            }
            _ if ext.is_empty() => crate::t!("mask.no_match"),
            _ => crate::t!("mask.ext_no_match", ext = ext),
        },
        (true, true) => match pos {
            Some(p) => crate::t!("mask.excluded_by", item = p.item),
            None => crate::t!("mask.excluded"),
        },
        (true, false) => crate::t!("mask.not_triggered"),
    };
    ItemCheck {
        item: item.as_string(),
        negated,
        hit,
        expanded,
        children,
        reason,
    }
}

/// One item of a [`MaskCheck`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ItemCheck {
    /// The item as written (`*.jpg`, `@png`, `!@raw`).
    pub item: String,
    pub negated: bool,
    /// The pattern behind the item (without its `!`) matches the name.
    pub hit: bool,
    /// A reference expanded (`*.{mp4,m4v} & !*.av1.mp4`); empty otherwise.
    pub expanded: String,
    /// The items of the referenced mask, judged the same way (recursively).
    pub children: Vec<ItemCheck>,
    /// One Russian phrase: why the item did or did not take the name.
    pub reason: String,
}

/// A mask expression judged against one file name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MaskCheck {
    pub source: String,
    /// The expression with `@refs` expanded.
    pub expanded: String,
    pub matched: bool,
    pub items: Vec<ItemCheck>,
}

/// A mask slot: one or several items. A name matches when it matches at least
/// one positive item (OR) and no `!` item; a list of only `!` items means
/// "everything except those". In TOML it is a string or a list of strings; a
/// string starting with `@` refers to a named mask.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MaskExpr(pub Vec<MaskItem>);

impl MaskExpr {
    /// A single pattern, `@name`, or `!` exclusion.
    pub fn parse(s: &str) -> Result<MaskExpr> {
        Ok(MaskExpr(vec![MaskItem::parse(s)?]))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The `@name`s this expression refers to directly (also under `!`).
    pub fn refs(&self) -> impl Iterator<Item = &str> {
        self.0.iter().filter_map(MaskItem::ref_name)
    }

    /// Whether any top-level item is a `!` exclusion.
    pub fn has_negation(&self) -> bool {
        self.0.iter().any(MaskItem::is_negated)
    }

    /// Like [`matches`](Self::matches) for lists that turn files away
    /// (`exclude`, `skip_if`): an unresolvable reference counts as a hit, so
    /// a broken config leaves the file alone instead of taking more.
    pub fn matches_veto(&self, file_name: &str, scope: &(impl MaskScope + ?Sized)) -> bool {
        if self.0.is_empty() {
            return false;
        }
        let hit = |i: &MaskItem| i.hits(file_name, scope, 0, true);
        if self.0.iter().any(|i| i.is_negated() && hit(i)) {
            return false;
        }
        let mut positives = self.0.iter().filter(|i| !i.is_negated()).peekable();
        positives.peek().is_none() || positives.any(hit)
    }

    /// Whether the expression takes the name. An unknown positive reference
    /// matches nothing and an unknown `!` reference excludes everything
    /// (validation reports both).
    pub fn matches(&self, file_name: &str, scope: &(impl MaskScope + ?Sized)) -> bool {
        self.matches_at(file_name, scope, 0)
    }

    fn matches_at(&self, file_name: &str, scope: &(impl MaskScope + ?Sized), depth: usize) -> bool {
        if self.0.is_empty() {
            return false;
        }
        let excluded = self
            .0
            .iter()
            .any(|i| matches!(i, MaskItem::Not(_)) && i.hits(file_name, scope, depth, true));
        if excluded {
            return false;
        }
        let mut positives = self.0.iter().filter(|i| !i.is_negated()).peekable();
        positives.peek().is_none() || positives.any(|i| i.hits(file_name, scope, depth, false))
    }

    /// How the expression judges `file_name`, item by item: the same
    /// verdict as [`matches`](Self::matches) (or, with `veto`, as
    /// [`matches_veto`](Self::matches_veto)), plus which items hit.
    pub fn check(
        &self,
        file_name: &str,
        scope: &(impl MaskScope + ?Sized),
        veto: bool,
    ) -> MaskCheck {
        let items = self
            .0
            .iter()
            .map(|i| check_item(i, file_name, scope, 0, veto))
            .collect();
        MaskCheck {
            source: self.source(),
            expanded: self.expand(scope),
            matched: if veto {
                self.matches_veto(file_name, scope)
            } else {
                self.matches(file_name, scope)
            },
            items,
        }
    }

    /// The expression as written: positive items joined with ` | `, every
    /// `!` item appended as ` & !item`.
    pub fn source(&self) -> String {
        Self::join(self.0.iter().map(MaskItem::as_string).collect(), &self.0)
    }

    fn join(parts: Vec<String>, items: &[MaskItem]) -> String {
        let mut pos = Vec::new();
        let mut neg = Vec::new();
        for (p, i) in parts.into_iter().zip(items) {
            if i.is_negated() {
                neg.push(p)
            } else {
                pos.push(p)
            }
        }
        let mut out = pos.join(" | ");
        for n in neg {
            if !out.is_empty() {
                out.push_str(" & ");
            }
            out.push_str(&n);
        }
        out
    }

    /// The expression with references expanded, for display. Unknown
    /// references stay as `@name`; a referenced expression that carries a
    /// `!` is parenthesised.
    pub fn expand(&self, scope: &(impl MaskScope + ?Sized)) -> String {
        self.expand_at(scope, 0)
    }

    fn expand_at(&self, scope: &(impl MaskScope + ?Sized), depth: usize) -> String {
        let one = |i: &MaskItem| -> String {
            let (neg, inner) = match i {
                MaskItem::Not(x) => (true, x.as_ref()),
                x => (false, x),
            };
            let text = match inner {
                MaskItem::Ref(n) if depth < MAX_REF_DEPTH => match scope.named_mask(n) {
                    Some(e) => {
                        let t = e.expand_at(scope, depth + 1);
                        let compound = e.0.len() > 1;
                        // `a | b` and `a & !b` must stay one unit.
                        let carries_not = compound && e.0.iter().any(MaskItem::is_negated);
                        if (neg && compound) || (carries_not && self.0.len() > 1) {
                            format!("({t})")
                        } else {
                            t
                        }
                    }
                    None => inner.as_string(),
                },
                _ => inner.as_string(),
            };
            if neg { format!("!{text}") } else { text }
        };
        let parts = self.0.iter().map(one).collect();
        Self::join(parts, &self.0)
    }
}

impl Serialize for MaskExpr {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        match self.0.as_slice() {
            [one] => ser.serialize_str(&one.as_string()),
            many => {
                let mut seq = ser.serialize_seq(Some(many.len()))?;
                for i in many {
                    seq.serialize_element(&i.as_string())?;
                }
                seq.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for MaskExpr {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = MaskExpr;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str(&crate::t!("mask.expecting"))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> std::result::Result<MaskExpr, E> {
                MaskExpr::parse(v).map_err(E::custom)
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<MaskExpr, A::Error> {
                let mut items = Vec::new();
                while let Some(s) = seq.next_element::<String>()? {
                    items.push(MaskItem::parse(&s).map_err(serde::de::Error::custom)?);
                }
                Ok(MaskExpr(items))
            }
        }
        de.deserialize_any(V)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(s: &str) -> Mask {
        Mask::parse(s).unwrap()
    }

    #[test]
    fn mask_glob_braces() {
        let k = m("*.{jpg,heic}");
        assert!(k.matches("a.JPG"));
        assert!(k.matches("b.heic"));
        assert!(!k.matches("a.png"));
    }

    #[test]
    fn mask_regex() {
        let k = m(r"^[0-9a-f]{32}.*");
        assert!(k.matches("0123456789abcdef0123456789abcdef.mp4"));
        assert!(!k.matches("photo.mp4"));
    }

    #[test]
    fn mask_case_and_no_ext() {
        assert!(m("*").matches("README"));
        assert!(m("*.jpeg").matches("IMG_1.JPEG"));
        assert!(m("*скрин*").matches("Скрин 1.png"));
    }

    #[test]
    fn mask_rejects_invalid() {
        assert!(Mask::parse("^(").is_err());
        assert!(Mask::parse("{a").is_err());
    }

    #[test]
    fn mask_serde_roundtrip() {
        #[derive(Serialize, Deserialize)]
        struct W {
            m: Mask,
        }
        let w: W = toml::from_str("m = \"*.{jpg,png}\"").unwrap();
        assert!(w.m.matches("x.PNG"));
        assert_eq!(toml::to_string(&w).unwrap().trim(), "m = \"*.{jpg,png}\"");
        assert!(toml::from_str::<W>("m = \"^(\"").is_err());
    }

    struct Scope(Vec<(&'static str, MaskExpr)>);
    impl MaskScope for Scope {
        fn named_mask(&self, name: &str) -> Option<&MaskExpr> {
            self.0.iter().find(|(n, _)| *n == name).map(|(_, e)| e)
        }
    }

    fn list(items: &[&str]) -> MaskExpr {
        MaskExpr(items.iter().map(|s| MaskItem::parse(s).unwrap()).collect())
    }

    #[test]
    fn negation_positive_and_negative() {
        let e = list(&["*.{mp4,mov}", "!*.av1.mp4"]);
        assert!(e.matches("a.mp4", &()) && e.matches("A.MOV", &()));
        assert!(!e.matches("a.av1.mp4", &()) && !e.matches("a.AV1.MP4", &()));
        assert!(!e.matches("a.png", &()));
        // The exclusion wins whatever the order.
        let e = list(&["!*.av1.mp4", "*.mp4"]);
        assert!(e.matches("a.mp4", &()) && !e.matches("a.av1.mp4", &()));
    }

    #[test]
    fn negation_only_means_everything_except() {
        let e = list(&["!*.tmp", "!^draft"]);
        assert!(e.matches("a.png", &()) && e.matches("README", &()));
        assert!(!e.matches("a.TMP", &()) && !e.matches("draft1.png", &()));
        assert!(MaskExpr::parse("!*.tmp").unwrap().matches("a.jpg", &()));
        assert!(!MaskExpr::parse("!*.tmp").unwrap().matches("a.tmp", &()));
        // An empty list still matches nothing.
        assert!(!MaskExpr::default().matches("a.jpg", &()));
    }

    #[test]
    fn negated_and_nested_refs() {
        let s = Scope(vec![
            ("raw", list(&["*.{cr2,nef}"])),
            ("vid", list(&["*.{mp4,mov}", "!*.av1.mp4"])),
            ("outer", list(&["@vid", "*.mkv"])),
            ("not-vid", list(&["!@vid"])),
        ]);
        let neg = list(&["*", "!@raw"]);
        assert!(neg.matches("a.jpg", &s) && !neg.matches("a.CR2", &s));
        // A referenced mask is evaluated as a whole, then used as one item.
        let outer = MaskExpr::parse("@outer").unwrap();
        assert!(outer.matches("a.mp4", &s) && outer.matches("a.mkv", &s));
        assert!(!outer.matches("a.av1.mp4", &s));
        let nv = MaskExpr::parse("@not-vid").unwrap();
        assert!(nv.matches("a.jpg", &s) && nv.matches("a.av1.mp4", &s));
        assert!(!nv.matches("a.mp4", &s));
        assert!(list(&["!@outer"]).matches("a.png", &s));
        assert!(!list(&["!@outer"]).matches("a.mkv", &s));
        // Unknown `!@x` fails safe: it excludes everything.
        assert!(!list(&["*", "!@nope"]).matches("a.jpg", &s));
        assert!(!list(&["@nope"]).matches("a.jpg", &s));
    }

    #[test]
    fn unresolved_ref_in_a_veto_list_counts_as_a_hit() {
        let e = list(&["@nope"]);
        assert!(!e.matches("a.jpg", &()));
        assert!(e.matches_veto("a.jpg", &()));
        let ok = list(&["*.cr2"]);
        assert!(ok.matches_veto("a.cr2", &()) && !ok.matches_veto("a.jpg", &()));
        assert!(list(&["*.cr2", "@nope"]).matches_veto("a.jpg", &()));
        assert!(list(&["a", "!b"]).has_negation() && !ok.has_negation());
    }

    #[test]
    fn negation_validation_errors() {
        for bad in [
            "!", "!  ", "!@", "!!x", "!^(", "!{a", "! x", "!\t*.jpg", "! @raw",
        ] {
            assert!(MaskItem::parse(bad).is_err(), "{bad}");
            assert!(MaskExpr::parse(bad).is_err(), "{bad}");
        }
        assert!(MaskItem::parse("!*.png").unwrap().is_negated());
        assert_eq!(list(&["a", "!@r"]).refs().collect::<Vec<_>>(), ["r"]);
    }

    #[test]
    fn negation_serde_roundtrip() {
        #[derive(Serialize, Deserialize)]
        struct W {
            m: MaskExpr,
            l: MaskExpr,
        }
        let w: W = toml::from_str("m = \"!*.av1.mp4\"\nl = [\"*.mp4\", \"!@raw\"]").unwrap();
        assert!(w.m.matches("a.mp4", &()) && !w.m.matches("a.av1.mp4", &()));
        assert_eq!(
            toml::to_string(&w).unwrap().trim(),
            "m = \"!*.av1.mp4\"\nl = [\"*.mp4\", \"!@raw\"]"
        );
        assert!(toml::from_str::<W>("m = \"!\"\nl = []").is_err());
        assert!(toml::from_str::<W>("m = \"*\"\nl = [\"!\"]").is_err());
    }

    #[test]
    fn source_and_expand_show_exclusions() {
        let s = Scope(vec![
            ("raw", list(&["*.cr2", "*.nef"])),
            ("vid", list(&["*.mp4", "!*.av1.mp4"])),
        ]);
        let e = list(&["*.png", "@raw", "!*.tmp"]);
        assert_eq!(e.source(), "*.png | @raw & !*.tmp");
        assert_eq!(e.expand(&s), "*.png | *.cr2 | *.nef & !*.tmp");
        // Alone, a reference needs no parentheses.
        assert_eq!(list(&["@vid"]).expand(&s), "*.mp4 & !*.av1.mp4");
        assert_eq!(
            list(&["@vid", "*.mkv"]).expand(&s),
            "(*.mp4 & !*.av1.mp4) | *.mkv"
        );
        assert_eq!(list(&["!@raw"]).expand(&s), "!(*.cr2 | *.nef)");
        assert_eq!(list(&["!@vid"]).expand(&s), "!(*.mp4 & !*.av1.mp4)");
        assert_eq!(list(&["!@nope"]).expand(&s), "!@nope");
    }
}
