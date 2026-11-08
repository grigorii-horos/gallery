//! The user's presets and masks in the document: `[name_presets.*]`,
//! `[content_presets.*]` and `[masks]` read into drafts, written back key
//! by key (untouched keys, comments and order stay as written), and the
//! operations on them: add, update, remove (refused while something uses
//! it), rename (optionally with every reference in the config) and "copy a
//! built-in as my own".

use std::collections::BTreeMap;

use gallery_core::config::{ContentPreset, NamePreset};
use gallery_core::mask::MaskExpr;
use gallery_core::presets::{builtin_content_preset, builtin_mask, builtin_name_preset};
use serde::Serialize;
use toml_edit::{DocumentMut, Item, Key, Table, TableLike, Value};

use super::doc::{
    Style, bool_at, mask_at, mask_value, put, put_mask, read_draft, steps_at, steps_val, str_at,
    write_target,
};
use super::model::{
    ConfigDraft, ContentPresetDraft, MaskDraft, NamePresetDraft, PresetKind, StepDraft, TargetDraft,
};
use super::order::{Seg, TablePath, render_order, renumber, split_prefix, table, table_mut};

type R<T> = Result<T, String>;

// ---------- content preset fields ----------

/// One field of a content preset draft, typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Slot {
    B(Option<bool>),
    I(Option<i64>),
    S(Option<String>),
}

impl Slot {
    fn value(&self) -> Option<Value> {
        match self {
            Slot::B(v) => v.map(Value::from),
            Slot::I(v) => v.map(Value::from),
            Slot::S(v) => v.as_deref().map(Value::from),
        }
    }

    fn cleared(&self) -> Slot {
        match self {
            Slot::B(_) => Slot::B(None),
            Slot::I(_) => Slot::I(None),
            Slot::S(_) => Slot::S(None),
        }
    }

    /// The same slot holding `item`'s value; `None` when the type differs.
    fn read(&self, item: &Item) -> Option<Slot> {
        Some(match self {
            Slot::B(_) => Slot::B(Some(item.as_bool()?)),
            Slot::I(_) => Slot::I(Some(item.as_integer()?)),
            Slot::S(_) => Slot::S(Some(item.as_str()?.to_string())),
        })
    }

    /// The same slot holding a JSON value (of a built-in preset).
    fn with_json(&self, v: &serde_json::Value) -> Slot {
        match self {
            Slot::B(_) => Slot::B(v.as_bool()),
            Slot::I(_) => Slot::I(v.as_i64()),
            Slot::S(_) => Slot::S(v.as_str().map(str::to_string)),
        }
    }
}

macro_rules! content_fields {
    ($($key:ident : $ty:ident),* $(,)?) => {
        /// Every field key of every kind, in the order new tables get them.
        pub const ALL_KEYS: &[&str] = &[$(stringify!($key)),*];

        impl ContentPresetDraft {
            pub fn slot(&self, key: &str) -> Option<Slot> {
                match key {
                    $(stringify!($key) => Some(Slot::$ty(self.$key.clone())),)*
                    _ => None,
                }
            }

            fn set_slot(&mut self, key: &str, s: Slot) {
                match (key, s) {
                    $((stringify!($key), Slot::$ty(v)) => self.$key = v,)*
                    _ => {}
                }
            }
        }
    };
}

content_fields! {
    write_capture_date: B,
    fix_gcam_make_model: B,
    strip_gcamera_xmp: B,
    strip_gain_map_xmp_for_avif: B,
    fill_offset_time: B,
    video_make_model: B,
    video_restore_gps: B,
    format: S,
    quality: I,
    speed: I,
    crf: I,
    preset: I,
    gop: I,
    max_short_side: I,
    audio_kbps: I,
    denoise: S,
    stabilize: B,
    ladder: B,
    min_saving_pct: I,
    codec: S,
    kbps: I,
    level: I,
}

/// The keys a content kind has (besides `kind` and `mask`).
pub fn kind_keys(kind: &str) -> &'static [&'static str] {
    match kind {
        "metadata" => &[
            "write_capture_date",
            "fix_gcam_make_model",
            "strip_gcamera_xmp",
            "strip_gain_map_xmp_for_avif",
            "fill_offset_time",
            "video_make_model",
            "video_restore_gps",
        ],
        "photo" => &[
            "format",
            "quality",
            "speed",
            "max_short_side",
            "ladder",
            "min_saving_pct",
        ],
        "video" => &[
            "crf",
            "preset",
            "gop",
            "max_short_side",
            "audio_kbps",
            "denoise",
            "stabilize",
            "ladder",
        ],
        "audio" => &["codec", "kbps"],
        "png" => &["level"],
        _ => &[],
    }
}

impl ContentPresetDraft {
    /// The field as it is written for this kind: a key of another kind is
    /// absent.
    fn written(&self, key: &str) -> Option<Slot> {
        let s = self.slot(key)?;
        Some(if kind_keys(&self.kind).contains(&key) {
            s
        } else {
            s.cleared()
        })
    }
}

// ---------- reading ----------

/// The table of `kind` (`[name_presets]`…), if any.
fn group(doc: &DocumentMut, kind: PresetKind) -> R<Option<&dyn TableLike>> {
    match doc.get(kind.key()) {
        None => Ok(None),
        Some(i) => i.as_table_like().map(Some).ok_or_else(|| {
            gallery_core::t!(
                "edit.expected",
                path = kind.key(),
                what = gallery_core::t!("edit.what.table")
            )
        }),
    }
}

fn entries(doc: &DocumentMut, kind: PresetKind) -> R<Vec<(String, &dyn TableLike)>> {
    let Some(g) = group(doc, kind)? else {
        return Ok(Vec::new());
    };
    g.iter()
        .map(|(k, item)| {
            item.as_table_like()
                .map(|t| (k.to_string(), t))
                .ok_or_else(|| {
                    gallery_core::t!(
                        "edit.expected",
                        path = format!("{}.{k}", kind.key()),
                        what = gallery_core::t!("edit.what.table")
                    )
                })
        })
        .collect()
}

fn read_name(name: String, t: &dyn TableLike, path: &str) -> R<NamePresetDraft> {
    Ok(NamePresetDraft {
        name,
        mask: mask_at(t, "mask", path)?,
        template: str_at(t, "template", path)?.unwrap_or_default(),
        date: str_at(t, "date", path)?,
        skip_if: mask_at(t, "skip_if", path)?,
        normalize_ext: bool_at(t, "normalize_ext", path)?,
    })
}

fn read_content(name: String, t: &dyn TableLike, path: &str) -> R<ContentPresetDraft> {
    let mut p = ContentPresetDraft {
        name,
        kind: str_at(t, "kind", path)?.unwrap_or_default(),
        mask: mask_at(t, "mask", path)?,
        steps: t
            .contains_key("steps")
            .then(|| steps_at(t, "steps", path))
            .transpose()?,
        ..Default::default()
    };
    for key in ALL_KEYS {
        if let Some(item) = t.get(key) {
            let slot = p.slot(key).expect("a known key");
            let v = slot.read(item).ok_or_else(|| {
                let what = match slot {
                    Slot::B(_) => gallery_core::t!("edit.what.bool"),
                    Slot::I(_) => gallery_core::t!("edit.what.int"),
                    Slot::S(_) => gallery_core::t!("edit.what.string"),
                };
                gallery_core::t!("edit.expected", path = format!("{path}.{key}"), what = what)
            })?;
            p.set_slot(key, v);
        }
    }
    Ok(p)
}

/// The user's name presets, content presets and masks, in file order.
pub fn read(
    doc: &DocumentMut,
) -> R<(
    Vec<NamePresetDraft>,
    Vec<ContentPresetDraft>,
    Vec<MaskDraft>,
)> {
    let names = entries(doc, PresetKind::Name)?
        .into_iter()
        .map(|(n, t)| {
            let p = format!("name_presets.{n}");
            read_name(n, t, &p)
        })
        .collect::<R<_>>()?;
    let contents = entries(doc, PresetKind::Content)?
        .into_iter()
        .map(|(n, t)| {
            let p = format!("content_presets.{n}");
            read_content(n, t, &p)
        })
        .collect::<R<_>>()?;
    let masks = match group(doc, PresetKind::Mask)? {
        None => Vec::new(),
        Some(g) => g
            .iter()
            .map(|(k, item)| {
                let p = format!("masks.{k}");
                let v = item.as_value().ok_or_else(|| {
                    gallery_core::t!(
                        "edit.expected",
                        path = p,
                        what = gallery_core::t!("edit.what.mask")
                    )
                })?;
                Ok(MaskDraft {
                    name: k.to_string(),
                    items: mask_value(v, &p)?,
                })
            })
            .collect::<R<_>>()?,
    };
    Ok((names, contents, masks))
}

// ---------- writing ----------

pub fn write_name(t: &mut Table, old: &NamePresetDraft, new: &NamePresetDraft) {
    if old.template != new.template {
        put(t, "template", Some(new.template.as_str().into()));
    }
    if old.date != new.date {
        put(t, "date", new.date.as_deref().map(Value::from));
    }
    if old.skip_if != new.skip_if {
        put_mask(t, "skip_if", new.skip_if.as_deref());
    }
    if old.normalize_ext != new.normalize_ext {
        put(t, "normalize_ext", new.normalize_ext.map(Value::from));
    }
    if old.mask != new.mask {
        put_mask(t, "mask", new.mask.as_deref());
    }
}

/// Write content preset `old` as `new`: the keys of `new`'s kind, and no
/// key of another kind.
pub fn write_content(t: &mut Table, old: &ContentPresetDraft, new: &ContentPresetDraft) {
    if old.kind != new.kind {
        put(t, "kind", Some(new.kind.as_str().into()));
    }
    for key in ALL_KEYS {
        let had = old.slot(key);
        let want = new.written(key);
        if had != want {
            put(t, key, want.and_then(|s| s.value()));
        }
    }
    // Only a bundle has steps.
    let steps = |p: &ContentPresetDraft| p.steps.clone().filter(|_| p.kind == "bundle");
    if steps(old) != steps(new) {
        put(t, "steps", steps(new).as_deref().map(steps_val));
    }
    if old.mask != new.mask {
        put_mask(t, "mask", new.mask.as_deref());
    }
}

fn group_mut(doc: &mut DocumentMut, kind: PresetKind) -> R<Option<&mut Table>> {
    match doc.get_mut(kind.key()) {
        None => Ok(None),
        Some(Item::Table(t)) => Ok(Some(t)),
        Some(_) => Err(gallery_core::t!("edit.group_not_table", key = kind.key())),
    }
}

/// Run `f` on the table of preset `name` (an inline `{ … }` one is
/// rewritten from a table).
fn with_entry(
    doc: &mut DocumentMut,
    kind: PresetKind,
    name: &str,
    f: impl FnOnce(&mut Table),
) -> R<()> {
    let g = group_mut(doc, kind)?.ok_or_else(|| missing(kind, name))?;
    match g.get_mut(name) {
        Some(Item::Table(t)) => f(t),
        Some(Item::Value(Value::InlineTable(it))) => {
            let decor = it.decor().clone();
            let mut t = it.clone().into_table();
            f(&mut t);
            let mut n = t.into_inline_table();
            *n.decor_mut() = decor;
            *it = n;
        }
        Some(_) => {
            return Err(gallery_core::t!(
                "edit.expected",
                path = format!("{}.{name}", kind.key()),
                what = gallery_core::t!("edit.what.table")
            ));
        }
        None => return Err(missing(kind, name)),
    }
    Ok(())
}

fn missing(kind: PresetKind, name: &str) -> String {
    gallery_core::t!(
        match kind {
            PresetKind::Name => "edit.name_preset_missing",
            PresetKind::Content => "edit.content_preset_missing",
            PresetKind::Mask => "edit.mask_missing",
        },
        name = name
    )
}

fn is_targets(p: &TablePath) -> bool {
    matches!(p.first(), Some(Seg::Key(k) | Seg::Idx(k, _)) if k == "targets")
}

/// Where a new table of `kind` goes: after the last one of its kind, else
/// right before the first target, else at the end of the file.
fn new_position(doc: &mut DocumentMut, kind: PresetKind) -> isize {
    renumber(doc);
    let order = render_order(doc);
    let explicit = |p: &TablePath| table(doc, p).is_some_and(|t| !t.is_implicit());
    let own = order
        .iter()
        .filter(|(_, p)| matches!(p.first(), Some(Seg::Key(k)) if k == kind.key()))
        .filter(|(_, p)| explicit(p))
        .map(|(pos, _)| *pos)
        .max();
    if let Some(p) = own {
        return p + 1;
    }
    match order.iter().find(|(_, p)| is_targets(p) && explicit(p)) {
        Some((pos, _)) => pos - 1,
        None => order.last().map_or(1, |(p, _)| p + 1),
    }
}

/// Add preset table `fresh` named `name` to the group of `kind`.
fn add_table(doc: &mut DocumentMut, kind: PresetKind, name: &str, mut fresh: Table) -> R<()> {
    group_mut(doc, kind)?;
    let pos = new_position(doc, kind);
    if doc.get(kind.key()).is_none() {
        let mut g = Table::new();
        g.set_implicit(true);
        doc.insert(kind.key(), Item::Table(g));
    }
    let g = group_mut(doc, kind)?.expect("made above");
    // A group written as `[name_presets]` with `x = { … }` lines gets one more line.
    if !g.is_implicit() && g.iter().any(|(_, i)| i.is_value()) {
        g.insert(
            name,
            Item::Value(Value::InlineTable(fresh.into_inline_table())),
        );
    } else {
        fresh.set_position(Some(pos));
        fresh.decor_mut().set_prefix("\n");
        g.insert(name, Item::Table(fresh));
    }
    Ok(())
}

fn add_mask(doc: &mut DocumentMut, m: &MaskDraft) -> R<()> {
    group_mut(doc, PresetKind::Mask)?;
    if doc.get("masks").is_none() {
        let pos = new_position(doc, PresetKind::Mask);
        let mut g = Table::new();
        g.set_position(Some(pos));
        g.decor_mut().set_prefix("\n");
        doc.insert("masks", Item::Table(g));
    }
    let g = group_mut(doc, PresetKind::Mask)?.expect("made above");
    put_mask(g, &m.name, Some(&m.items));
    Ok(())
}

/// Remove table `name` of `kind`; a section heading above it (comments up
/// to a blank line) moves to the table that follows.
fn remove_table(doc: &mut DocumentMut, kind: PresetKind, name: &str) -> R<()> {
    renumber(doc);
    let order = render_order(doc);
    let path = vec![Seg::Key(kind.key().into()), Seg::Key(name.into())];
    let at = order.iter().position(|(_, p)| *p == path);
    let g = group_mut(doc, kind)?.ok_or_else(|| missing(kind, name))?;
    let item = g.remove(name).ok_or_else(|| missing(kind, name))?;
    let implicit_empty = g.is_implicit() && g.is_empty();
    if implicit_empty {
        doc.remove(kind.key());
    }
    let Item::Table(t) = item else {
        return Ok(());
    };
    let prefix = t
        .decor()
        .prefix()
        .and_then(|r| r.as_str())
        .unwrap_or("")
        .to_string();
    let (place, _) = split_prefix(&prefix);
    if place.trim().is_empty() {
        return Ok(());
    }
    let next = at.and_then(|i| order.get(i + 1)).map(|(_, p)| p.clone());
    match next.and_then(|p| table_mut(doc, &p)) {
        Some(n) => {
            let cur = n
                .decor()
                .prefix()
                .and_then(|r| r.as_str())
                .unwrap_or("\n")
                .to_string();
            let new = if cur.trim().is_empty() {
                place
            } else {
                format!("{place}{}", cur.trim_start_matches('\n'))
            };
            n.decor_mut().set_prefix(new);
        }
        None => {
            let tail = doc.trailing().as_str().unwrap_or("").to_string();
            doc.set_trailing(format!("{place}{tail}"));
        }
    }
    Ok(())
}

/// Rename key `from` of `t` to `to` in place (its order, decor and value
/// kept).
fn rename_key(t: &mut Table, from: &str, to: &str) {
    let keys: Vec<String> = t.iter().map(|(k, _)| k.to_string()).collect();
    let all: Vec<(Key, Item)> = keys.iter().filter_map(|k| t.remove_entry(k)).collect();
    for (key, item) in all {
        let key = if key.get() == from {
            Key::new(to)
                .with_leaf_decor(key.leaf_decor().clone())
                .with_dotted_decor(key.dotted_decor().clone())
        } else {
            key
        };
        t.insert_formatted(&key, item);
    }
}

// ---------- references ----------

fn mask_has(m: &[String], name: &str) -> bool {
    let r = format!("@{name}");
    m.contains(&r)
}

fn swap_ref(m: &mut [String], from: &str, to: &str) {
    let (f, t) = (format!("@{from}"), format!("@{to}"));
    for x in m.iter_mut() {
        if *x == f {
            *x = t.clone();
        }
    }
}

/// Where in the config the user's `kind` item `name` is used, as lines for
/// the UI ("target “Timeline”, output 1: content").
pub fn references(d: &ConfigDraft, kind: PresetKind, name: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |s: String| {
        if !out.contains(&s) {
            out.push(s);
        }
    };
    for (_, _, t) in d.targets.all() {
        let tn = &t.name;
        if kind == PresetKind::Mask {
            for (i, f) in t.filters.iter().enumerate() {
                if f.matches.as_deref().is_some_and(|m| mask_has(m, name))
                    || mask_has(&f.exclude, name)
                {
                    push(gallery_core::t!("edit.ref_filter", target = tn, n = i + 1));
                }
            }
            if let Some(p) = &t.prefilter
                && (p.matches.as_deref().is_some_and(|m| mask_has(m, name))
                    || mask_has(&p.exclude, name))
            {
                push(gallery_core::t!("edit.ref_prefilter", target = tn));
            }
        }
        for (i, o) in t.outputs.iter().enumerate() {
            for (pipe, steps, title) in [
                (
                    PresetKind::Content,
                    &o.content,
                    gallery_core::t!("explain.kind_content"),
                ),
                (
                    PresetKind::Name,
                    &o.rename,
                    gallery_core::t!("explain.kind_name"),
                ),
            ] {
                for s in steps {
                    let hit = match kind {
                        PresetKind::Mask => s.mask.as_deref().is_some_and(|m| mask_has(m, name)),
                        k => k == pipe && s.preset == name,
                    };
                    if hit {
                        push(gallery_core::t!(
                            "edit.ref_output",
                            target = tn,
                            n = i + 1,
                            title = title
                        ));
                    }
                }
            }
        }
    }
    if kind == PresetKind::Content {
        for p in &d.content_presets {
            if p.steps.iter().flatten().any(|s| s.preset == name) {
                push(format!("set “{}”", p.name));
            }
        }
    }
    if kind == PresetKind::Mask {
        for p in &d.content_presets {
            let uses = p
                .steps
                .iter()
                .flatten()
                .any(|s| s.mask.as_deref().is_some_and(|m| mask_has(m, name)));
            if uses {
                push(format!("set “{}”", p.name));
            }
        }
        for m in &d.mask_defs {
            if m.name != name && mask_has(&m.items, name) {
                push(gallery_core::t!("edit.ref_mask", name = m.name));
            }
        }
        for p in &d.name_presets {
            let uses = [&p.mask, &p.skip_if]
                .into_iter()
                .any(|m| m.as_deref().is_some_and(|m| mask_has(m, name)));
            if uses {
                push(gallery_core::t!("config.who_name_preset", name = p.name));
            }
        }
        for p in &d.content_presets {
            if p.mask.as_deref().is_some_and(|m| mask_has(m, name)) {
                push(gallery_core::t!("config.who_content_preset", name = p.name));
            }
        }
    }
    out
}

/// For every user item, where it is used (only the used ones are listed).
#[derive(Debug, Clone, Default, Serialize)]
pub struct Usage {
    pub name: BTreeMap<String, Vec<String>>,
    pub content: BTreeMap<String, Vec<String>>,
    pub masks: BTreeMap<String, Vec<String>>,
}

pub fn usage(d: &ConfigDraft) -> Usage {
    let collect = |kind: PresetKind, names: Vec<&String>| {
        names
            .into_iter()
            .map(|n| (n.clone(), references(d, kind, n)))
            .filter(|(_, r)| !r.is_empty())
            .collect()
    };
    Usage {
        name: collect(
            PresetKind::Name,
            d.name_presets.iter().map(|p| &p.name).collect(),
        ),
        content: collect(
            PresetKind::Content,
            d.content_presets.iter().map(|p| &p.name).collect(),
        ),
        masks: collect(
            PresetKind::Mask,
            d.mask_defs.iter().map(|m| &m.name).collect(),
        ),
    }
}

fn rename_in_steps(steps: &mut [StepDraft], from: &str, to: &str) {
    for s in steps {
        if s.preset == from {
            s.preset = to.to_string();
        }
    }
}

fn rename_in_target(t: &mut TargetDraft, kind: PresetKind, from: &str, to: &str) {
    for o in &mut t.outputs {
        match kind {
            PresetKind::Name => rename_in_steps(&mut o.rename, from, to),
            PresetKind::Content => rename_in_steps(&mut o.content, from, to),
            PresetKind::Mask => {
                for s in o.content.iter_mut().chain(o.rename.iter_mut()) {
                    if let Some(m) = &mut s.mask {
                        swap_ref(m, from, to);
                    }
                }
            }
        }
    }
    if kind == PresetKind::Mask {
        for f in &mut t.filters {
            if let Some(m) = &mut f.matches {
                swap_ref(m, from, to);
            }
            swap_ref(&mut f.exclude, from, to);
        }
        if let Some(p) = &mut t.prefilter {
            if let Some(m) = &mut p.matches {
                swap_ref(m, from, to);
            }
            swap_ref(&mut p.exclude, from, to);
        }
    }
}

/// Point every reference to `from` at `to`.
fn rename_refs(doc: &mut DocumentMut, kind: PresetKind, from: &str, to: &str) -> R<()> {
    let d = read_draft(doc)?;
    let style = Style::of(doc);
    for (stage, i, t) in d.targets.all() {
        let mut n = t.clone();
        rename_in_target(&mut n, kind, from, to);
        if n != *t {
            renumber(doc);
            let tb = super::ops::stage_table(doc, stage, i)?;
            write_target(tb, stage, t, &n, &style);
        }
    }
    if kind == PresetKind::Content {
        for p in &d.content_presets {
            let mut n = p.clone();
            rename_in_steps(n.steps.as_deref_mut().unwrap_or_default(), from, to);
            if n != *p {
                with_entry(doc, PresetKind::Content, &p.name, |t| {
                    write_content(t, p, &n)
                })?;
            }
        }
    }
    if kind != PresetKind::Mask {
        return Ok(());
    }
    for m in &d.mask_defs {
        let mut items = m.items.clone();
        swap_ref(&mut items, from, to);
        if items != m.items {
            let g = group_mut(doc, PresetKind::Mask)?.expect("masks are there");
            put_mask(g, &m.name, Some(&items));
        }
    }
    for p in &d.name_presets {
        let mut n = p.clone();
        for m in [&mut n.mask, &mut n.skip_if].into_iter().flatten() {
            swap_ref(m, from, to);
        }
        if n != *p {
            with_entry(doc, PresetKind::Name, &p.name, |t| write_name(t, p, &n))?;
        }
    }
    for p in &d.content_presets {
        let mut n = p.clone();
        if let Some(m) = &mut n.mask {
            swap_ref(m, from, to);
        }
        for m in n.steps.iter_mut().flatten().filter_map(|s| s.mask.as_mut()) {
            swap_ref(m, from, to);
        }
        if n != *p {
            with_entry(doc, PresetKind::Content, &p.name, |t| {
                write_content(t, p, &n)
            })?;
        }
    }
    Ok(())
}

// ---------- built-ins ----------

fn items(e: &MaskExpr) -> Vec<String> {
    e.0.iter().map(|i| i.as_string()).collect()
}

fn content_mask(p: &ContentPreset) -> &Option<MaskExpr> {
    match p {
        ContentPreset::Metadata(p) => &p.mask,
        ContentPreset::Photo(p) => &p.mask,
        ContentPreset::Video(p) => &p.mask,
        ContentPreset::Audio(p) => &p.mask,
        ContentPreset::Png(p) => &p.mask,
        ContentPreset::Bundle(p) => &p.mask,
    }
}

pub fn name_draft_of(name: &str, p: &NamePreset) -> NamePresetDraft {
    NamePresetDraft {
        name: name.to_string(),
        mask: p.mask.as_ref().map(items),
        template: p.template.clone(),
        date: serde_json::to_value(p.date)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string)),
        skip_if: p.skip_if.as_ref().map(items),
        normalize_ext: Some(p.normalize_ext),
    }
}

pub fn content_draft_of(name: &str, p: &ContentPreset) -> ContentPresetDraft {
    let v = serde_json::to_value(p).unwrap_or_default();
    let kind = v["kind"].as_str().unwrap_or_default().to_string();
    let mut d = ContentPresetDraft {
        name: name.to_string(),
        mask: content_mask(p).as_ref().map(items),
        ..Default::default()
    };
    for key in kind_keys(&kind) {
        let s = d.slot(key).expect("a known key").with_json(&v[*key]);
        d.set_slot(key, s);
    }
    if let ContentPreset::Bundle(b) = p {
        d.steps = Some(
            b.steps
                .iter()
                .map(|s| StepDraft {
                    preset: s.preset.clone(),
                    mask: s.mask.as_ref().map(items),
                })
                .collect(),
        );
    }
    d.kind = kind;
    d
}

// ---------- operations ----------

fn is_builtin(kind: PresetKind, name: &str) -> bool {
    match kind {
        PresetKind::Name => builtin_name_preset(name).is_some(),
        PresetKind::Content => builtin_content_preset(name).is_some(),
        PresetKind::Mask => builtin_mask(name).is_some(),
    }
}

fn user_names(d: &ConfigDraft, kind: PresetKind) -> Vec<&str> {
    match kind {
        PresetKind::Name => d.name_presets.iter().map(|p| p.name.as_str()).collect(),
        PresetKind::Content => d.content_presets.iter().map(|p| p.name.as_str()).collect(),
        PresetKind::Mask => d.mask_defs.iter().map(|m| m.name.as_str()).collect(),
    }
}

/// A name a new (or renamed) item of `kind` may take.
fn check_new_name(doc: &DocumentMut, kind: PresetKind, name: &str) -> R<()> {
    let mask = kind == PresetKind::Mask;
    if name.trim().is_empty() {
        return Err(gallery_core::t!(if mask {
            "edit.mask_needs_name"
        } else {
            "edit.preset_needs_name"
        }));
    }
    if name.trim() != name {
        return Err(gallery_core::t!(
            if mask {
                "edit.mask_name_spaces"
            } else {
                "edit.preset_name_spaces"
            },
            name = name
        ));
    }
    if mask && (name.contains('@') || name.contains(char::is_whitespace)) {
        return Err(gallery_core::t!("edit.mask_name_chars", name = name));
    }
    if is_builtin(kind, name) {
        return Err(gallery_core::t!(
            if mask {
                "edit.builtin_mask_name"
            } else {
                "edit.builtin_preset_name"
            },
            name = name
        ));
    }
    if user_names(&read_draft(doc)?, kind).contains(&name) {
        return Err(gallery_core::t!(
            match kind {
                PresetKind::Name => "edit.name_preset_exists",
                PresetKind::Content => "edit.content_preset_exists",
                PresetKind::Mask => "edit.mask_exists",
            },
            name = name
        ));
    }
    Ok(())
}

fn exists(doc: &DocumentMut, kind: PresetKind, name: &str) -> R<()> {
    if user_names(&read_draft(doc)?, kind).contains(&name) {
        Ok(())
    } else {
        Err(missing(kind, name))
    }
}

pub fn add_name(doc: &mut DocumentMut, p: &NamePresetDraft) -> R<()> {
    check_new_name(doc, PresetKind::Name, &p.name)?;
    let mut t = Table::new();
    write_name(&mut t, &NamePresetDraft::default(), p);
    add_table(doc, PresetKind::Name, &p.name, t)
}

pub fn add_content(doc: &mut DocumentMut, p: &ContentPresetDraft) -> R<()> {
    check_new_name(doc, PresetKind::Content, &p.name)?;
    let mut t = Table::new();
    write_content(&mut t, &ContentPresetDraft::default(), p);
    add_table(doc, PresetKind::Content, &p.name, t)
}

pub fn add_mask_op(doc: &mut DocumentMut, m: &MaskDraft) -> R<()> {
    check_new_name(doc, PresetKind::Mask, &m.name)?;
    add_mask(doc, m)
}

pub fn update_name(doc: &mut DocumentMut, name: &str, p: &NamePresetDraft) -> R<()> {
    let old = read_draft(doc)?
        .name_presets
        .into_iter()
        .find(|x| x.name == name)
        .ok_or_else(|| missing(PresetKind::Name, name))?;
    with_entry(doc, PresetKind::Name, name, |t| write_name(t, &old, p))
}

pub fn update_content(doc: &mut DocumentMut, name: &str, p: &ContentPresetDraft) -> R<()> {
    let old = read_draft(doc)?
        .content_presets
        .into_iter()
        .find(|x| x.name == name)
        .ok_or_else(|| missing(PresetKind::Content, name))?;
    with_entry(doc, PresetKind::Content, name, |t| {
        write_content(t, &old, p)
    })
}

pub fn update_mask(doc: &mut DocumentMut, name: &str, m: &MaskDraft) -> R<()> {
    exists(doc, PresetKind::Mask, name)?;
    let g = group_mut(doc, PresetKind::Mask)?.ok_or_else(|| missing(PresetKind::Mask, name))?;
    let old = g
        .get(name)
        .and_then(Item::as_value)
        .map(|v| mask_value(v, name));
    if old.is_some_and(|o| o.as_ref() == Ok(&m.items)) {
        return Ok(());
    }
    put_mask(g, name, Some(&m.items));
    Ok(())
}

/// Remove `name` of `kind`; refused (with where it is used) while the
/// config refers to it.
pub fn remove(doc: &mut DocumentMut, kind: PresetKind, name: &str) -> R<()> {
    let d = read_draft(doc)?;
    if !user_names(&d, kind).contains(&name) {
        return Err(missing(kind, name));
    }
    let refs = references(&d, kind, name);
    if !refs.is_empty() {
        return Err(gallery_core::t!(
            match kind {
                PresetKind::Name => "edit.name_preset_in_use",
                PresetKind::Content => "edit.content_preset_in_use",
                PresetKind::Mask => "edit.mask_in_use",
            },
            name = name,
            refs = refs.join("; ")
        ));
    }
    match kind {
        PresetKind::Mask => {
            let g = group_mut(doc, kind)?.ok_or_else(|| missing(kind, name))?;
            g.remove(name);
            // The last mask takes its `[masks]` table with it.
            if g.is_empty() {
                doc.remove(kind.key());
            }
            Ok(())
        }
        _ => remove_table(doc, kind, name),
    }
}

pub fn rename(
    doc: &mut DocumentMut,
    kind: PresetKind,
    from: &str,
    to: &str,
    update_refs: bool,
) -> R<()> {
    exists(doc, kind, from)?;
    if from == to {
        return Ok(());
    }
    check_new_name(doc, kind, to)?;
    let g = group_mut(doc, kind)?.ok_or_else(|| missing(kind, from))?;
    rename_key(g, from, to);
    if update_refs {
        rename_refs(doc, kind, from, to)?;
    }
    Ok(())
}

pub fn copy_builtin(doc: &mut DocumentMut, kind: PresetKind, from: &str, to: &str) -> R<()> {
    let unknown = || {
        gallery_core::t!(
            match kind {
                PresetKind::Name => "edit.name_preset_not_builtin",
                PresetKind::Content => "edit.content_preset_not_builtin",
                PresetKind::Mask => "edit.mask_not_builtin",
            },
            name = from
        )
    };
    match kind {
        PresetKind::Name => {
            let p = builtin_name_preset(from).ok_or_else(unknown)?;
            add_name(doc, &name_draft_of(to, p))
        }
        PresetKind::Content => {
            let p = builtin_content_preset(from).ok_or_else(unknown)?;
            add_content(doc, &content_draft_of(to, p))
        }
        PresetKind::Mask => {
            let m = builtin_mask(from).ok_or_else(unknown)?;
            add_mask_op(
                doc,
                &MaskDraft {
                    name: to.to_string(),
                    items: items(m),
                },
            )
        }
    }
}
