//! The config file as a `toml_edit` document: drafts are read out of it, and
//! written back key by key. Only keys whose value changed are touched, so
//! comments, blank lines, key order and the formatting of everything else
//! stay as the user wrote them.

use toml_edit::{Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table, TableLike, Value};

use std::collections::BTreeMap;

use super::cond::{CONDITION_KEYS, condition_toml, item_json};
use super::model::{
    ConfigDraft, FilterDraft, OutputDraft, PlaceDraft, SourceDraft, StepDraft, TargetDraft,
    TargetKind,
};

type R<T> = Result<T, String>;

// ---------- reading ----------

/// `path: expected <what>`; `what` is a catalog key (`edit.what.*`).
fn wrong(path: &str, what: &str) -> String {
    gallery_core::t!("edit.expected", path = path, what = gallery_core::t!(what))
}

pub(super) fn str_at(t: &dyn TableLike, key: &str, path: &str) -> R<Option<String>> {
    match t.get(key) {
        None => Ok(None),
        Some(i) => i
            .as_str()
            .map(|s| Some(s.to_string()))
            .ok_or_else(|| wrong(&format!("{path}.{key}"), "edit.what.string")),
    }
}

pub(super) fn bool_at(t: &dyn TableLike, key: &str, path: &str) -> R<Option<bool>> {
    match t.get(key) {
        None => Ok(None),
        Some(i) => i
            .as_bool()
            .map(Some)
            .ok_or_else(|| wrong(&format!("{path}.{key}"), "edit.what.bool")),
    }
}

fn strings(v: &Value, path: &str) -> R<Vec<String>> {
    let a = v
        .as_array()
        .ok_or_else(|| wrong(path, "edit.what.strings"))?;
    a.iter()
        .map(|x| {
            x.as_str()
                .map(str::to_string)
                .ok_or_else(|| wrong(path, "edit.what.strings"))
        })
        .collect()
}

fn str_list_at(t: &dyn TableLike, key: &str, path: &str) -> R<Option<Vec<String>>> {
    let p = format!("{path}.{key}");
    match t.get(key) {
        None => Ok(None),
        Some(i) => i
            .as_value()
            .ok_or_else(|| wrong(&p, "edit.what.strings"))
            .and_then(|v| strings(v, &p))
            .map(Some),
    }
}

/// A mask slot: a string or a list of strings.
pub(super) fn mask_value(v: &Value, path: &str) -> R<Vec<String>> {
    match v.as_str() {
        Some(s) => Ok(vec![s.to_string()]),
        None => strings(v, path).map_err(|_| wrong(path, "edit.what.mask")),
    }
}

pub(super) fn mask_at(t: &dyn TableLike, key: &str, path: &str) -> R<Option<Vec<String>>> {
    let p = format!("{path}.{key}");
    match t.get(key) {
        None => Ok(None),
        Some(i) => i
            .as_value()
            .ok_or_else(|| wrong(&p, "edit.what.mask"))
            .and_then(|v| mask_value(v, &p))
            .map(Some),
    }
}

pub(super) fn steps_at(t: &dyn TableLike, key: &str, path: &str) -> R<Vec<StepDraft>> {
    let p = format!("{path}.{key}");
    let Some(item) = t.get(key) else {
        return Ok(Vec::new());
    };
    let arr = item
        .as_array()
        .ok_or_else(|| wrong(&p, "edit.what.steps"))?;
    arr.iter()
        .enumerate()
        .map(|(i, v)| {
            let sp = format!("{p}[{i}]");
            if let Some(s) = v.as_str() {
                return Ok(StepDraft {
                    preset: s.to_string(),
                    mask: None,
                });
            }
            let it = v
                .as_inline_table()
                .ok_or_else(|| wrong(&sp, "edit.what.step"))?;
            Ok(StepDraft {
                preset: str_at(it, "preset", &sp)?
                    .ok_or_else(|| gallery_core::t!("edit.step_no_preset_at", path = sp))?,
                mask: mask_at(it, "mask", &sp)?,
            })
        })
        .collect()
}

/// Tables under `key`: `[[key]]` headers or an inline array of tables.
fn tables_at<'a>(t: &'a dyn TableLike, key: &str, path: &str) -> R<Vec<&'a dyn TableLike>> {
    let p = format!("{path}.{key}");
    match t.get(key) {
        None => Ok(Vec::new()),
        Some(Item::ArrayOfTables(a)) => Ok(a.iter().map(|t| t as &dyn TableLike).collect()),
        Some(Item::Value(Value::Array(a))) => a
            .iter()
            .map(|v| {
                v.as_inline_table()
                    .map(|t| t as &dyn TableLike)
                    .ok_or_else(|| wrong(&p, "edit.what.tables"))
            })
            .collect(),
        Some(_) => Err(wrong(&p, "edit.what.tables")),
    }
}

fn source(t: &dyn TableLike, path: &str) -> R<SourceDraft> {
    Ok(SourceDraft {
        id: str_at(t, "id", path)?.unwrap_or_default(),
        path: str_at(t, "path", path)?.unwrap_or_default(),
        server_path: str_at(t, "server_path", path)?,
        recursive: bool_at(t, "recursive", path)?.unwrap_or(false),
        skip: str_list_at(t, "skip", path)?,
        ai_prefix: str_at(t, "ai_prefix", path)?,
        ai_prefix_file: str_at(t, "ai_prefix_file", path)?,
        ai_suffix: str_at(t, "ai_suffix", path)?,
        ai_suffix_file: str_at(t, "ai_suffix_file", path)?,
    })
}

fn filter(t: &dyn TableLike, path: &str) -> R<FilterDraft> {
    let conditions: BTreeMap<String, serde_json::Value> = CONDITION_KEYS
        .iter()
        .filter_map(|k| t.get(k).map(|i| (k.to_string(), item_json(i))))
        .collect();
    Ok(FilterDraft {
        sources: str_list_at(t, "sources", path)?.unwrap_or_default(),
        matches: mask_at(t, "match", path)?,
        exclude: mask_at(t, "exclude", path)?.unwrap_or_default(),
        ai: str_at(t, "ai", path)?,
        expand: str_at(t, "expand", path)?,
        conditions,
    })
}

fn kind_at(t: &dyn TableLike, path: &str) -> R<TargetKind> {
    match str_at(t, "kind", path)?.as_deref() {
        None | Some("folders") => Ok(TargetKind::Folders),
        Some("trash") => Ok(TargetKind::Trash),
        Some("keep") => Ok(TargetKind::Keep),
        Some(_) => Err(wrong(&format!("{path}.kind"), "edit.what.kind")),
    }
}

fn num_at(t: &dyn TableLike, key: &str, path: &str) -> R<f64> {
    t.get(key)
        .and_then(|i| i.as_float().or_else(|| i.as_integer().map(|x| x as f64)))
        .ok_or_else(|| wrong(&format!("{path}.{key}"), "edit.what.number"))
}

fn places(doc: &DocumentMut) -> R<Vec<PlaceDraft>> {
    let Some(p) = doc.get("places") else {
        return Ok(Vec::new());
    };
    let p = p
        .as_table_like()
        .ok_or_else(|| wrong("places", "edit.what.table"))?;
    p.iter()
        .map(|(name, item)| {
            let pp = format!("places.{name}");
            let t = item
                .as_table_like()
                .ok_or_else(|| wrong(&pp, "edit.what.table"))?;
            Ok(PlaceDraft {
                name: name.to_string(),
                lat: num_at(t, "lat", &pp)?,
                lon: num_at(t, "lon", &pp)?,
                km: num_at(t, "km", &pp)?,
            })
        })
        .collect()
}

fn output(t: &dyn TableLike, path: &str) -> R<OutputDraft> {
    Ok(OutputDraft {
        dir: str_at(t, "dir", path)?.unwrap_or_default(),
        server_dir: str_at(t, "server_dir", path)?,
        content: steps_at(t, "content", path)?,
        rename: steps_at(t, "rename", path)?,
    })
}

fn target(t: &dyn TableLike, path: &str) -> R<TargetDraft> {
    let filters = tables_at(t, "filters", path)?
        .into_iter()
        .enumerate()
        .map(|(i, f)| filter(f, &format!("{path}.filters[{i}]")))
        .collect::<R<_>>()?;
    let outputs = tables_at(t, "outputs", path)?
        .into_iter()
        .enumerate()
        .map(|(i, o)| output(o, &format!("{path}.outputs[{i}]")))
        .collect::<R<_>>()?;
    Ok(TargetDraft {
        name: str_at(t, "name", path)?.unwrap_or_default(),
        kind: kind_at(t, path)?,
        filters,
        outputs,
    })
}

/// The editable view of the whole document.
pub fn read_draft(doc: &DocumentMut) -> R<ConfigDraft> {
    let root = doc.as_table() as &dyn TableLike;
    let sources = tables_at(root, "sources", "")?
        .into_iter()
        .enumerate()
        .map(|(i, s)| source(s, &format!("sources[{i}]")))
        .collect::<R<_>>()?;
    // An old-shape `[targets]` table is migrated before it is read
    // (`super::migrate`): here it is not a list.
    let targets = tables_at(root, "targets", "")
        .map_err(|_| wrong("targets", "edit.what.tables"))?
        .into_iter()
        .enumerate()
        .map(|(i, x)| target(x, &format!("targets[{i}]")))
        .collect::<R<Vec<_>>>()?;
    let masks = doc
        .get("masks")
        .and_then(Item::as_table_like)
        .map(|m| m.iter().map(|(k, _)| k.to_string()).collect())
        .unwrap_or_default();
    let (name_presets, content_presets, mask_defs) = super::presets::read(doc)?;
    Ok(ConfigDraft {
        sources,
        places: places(doc)?,
        targets,
        masks,
        name_presets,
        content_presets,
        mask_defs,
    })
}

// ---------- writing values ----------

/// Set `key` to `new` (`None` removes it). A replaced value keeps the old
/// one's surrounding whitespace and trailing comment.
pub(super) fn put(t: &mut Table, key: &str, new: Option<Value>) {
    match (t.get_mut(key), new) {
        (Some(Item::Value(old)), Some(mut v)) => {
            if let Some(m) = multi_line_like(old, &v) {
                v = m;
            }
            *v.decor_mut() = old.decor().clone();
            *old = v;
        }
        (Some(item), Some(v)) => *item = Item::Value(v),
        (Some(_), None) => {
            t.remove(key);
        }
        (None, Some(v)) => {
            t.insert(key, Item::Value(v));
        }
        (None, None) => {}
    }
}

/// `new` written as a `"""` string when `old` was one and `new` has several
/// lines (checked by parsing it back).
fn multi_line_like(old: &Value, new: &Value) -> Option<Value> {
    let (Value::String(o), Value::String(n)) = (old, new) else {
        return None;
    };
    let was = o.as_repr()?.as_raw().as_str()?;
    let s = n.value();
    if !was.starts_with("\"\"\"") || !s.contains('\n') {
        return None;
    }
    let mut body = String::new();
    for c in s.chars() {
        match c {
            '\\' => body.push_str("\\\\"),
            '"' => body.push_str("\\\""),
            '\n' => body.push('\n'),
            c if c.is_control() => body.push_str(&format!("\\u{:04X}", c as u32)),
            c => body.push(c),
        }
    }
    let lit = format!("\"\"\"\n{body}\"\"\"");
    let doc: DocumentMut = format!("v = {lit}").parse().ok()?;
    let v = doc.get("v")?.as_value()?.clone();
    (v.as_str() == Some(s)).then_some(v)
}

pub(super) fn str_list(v: &[String]) -> Value {
    Value::Array(v.iter().map(String::as_str).collect::<Array>())
}

/// A mask as a string when it is one alternative (unless the file wrote a
/// list), else a list.
pub(super) fn mask_val(v: &[String], as_list: bool) -> Value {
    match v {
        [one] if !as_list => Value::from(one.as_str()),
        _ => str_list(v),
    }
}

fn is_list(t: &Table, key: &str) -> bool {
    t.get(key).and_then(Item::as_array).is_some()
}

pub(super) fn put_mask(t: &mut Table, key: &str, new: Option<&[String]>) {
    let as_list = is_list(t, key);
    put(t, key, new.map(|m| mask_val(m, as_list)));
}

pub(super) fn steps_val(steps: &[StepDraft]) -> Value {
    let mut a = Array::new();
    for s in steps {
        match &s.mask {
            None => a.push(s.preset.as_str()),
            Some(m) => {
                let mut it = InlineTable::new();
                it.insert("mask", mask_val(m, false));
                it.insert("preset", Value::from(s.preset.as_str()));
                a.push(Value::InlineTable(it));
            }
        }
    }
    Value::Array(a)
}

macro_rules! changed {
    ($old:expr, $new:expr, $field:ident) => {
        $old.$field != $new.$field
    };
}

pub fn write_source(t: &mut Table, old: &SourceDraft, new: &SourceDraft) {
    if changed!(old, new, id) {
        put(t, "id", Some(new.id.as_str().into()));
    }
    if changed!(old, new, path) {
        put(t, "path", Some(new.path.as_str().into()));
    }
    let opt = |s: &Option<String>| s.as_deref().map(Value::from);
    if changed!(old, new, server_path) {
        put(t, "server_path", opt(&new.server_path));
    }
    if changed!(old, new, recursive) {
        // `false` is the default: keep an explicit key, else leave it out.
        let keep = new.recursive || t.contains_key("recursive");
        put(t, "recursive", keep.then(|| Value::from(new.recursive)));
    }
    if changed!(old, new, skip) {
        put(t, "skip", new.skip.as_deref().map(str_list));
    }
    for (key, o, n) in [
        ("ai_prefix", &old.ai_prefix, &new.ai_prefix),
        ("ai_prefix_file", &old.ai_prefix_file, &new.ai_prefix_file),
        ("ai_suffix", &old.ai_suffix, &new.ai_suffix),
        ("ai_suffix_file", &old.ai_suffix_file, &new.ai_suffix_file),
    ] {
        if o != n {
            put(t, key, opt(n));
        }
    }
}

fn write_filter(t: &mut Table, old: &FilterDraft, new: &FilterDraft) {
    if changed!(old, new, sources) {
        put(t, "sources", Some(str_list(&new.sources)));
    }
    if changed!(old, new, matches) {
        put_mask(t, "match", new.matches.as_deref());
    }
    if changed!(old, new, exclude) {
        put_mask(
            t,
            "exclude",
            Some(&new.exclude).filter(|e| !e.is_empty()).map(|e| &e[..]),
        );
    }
    if changed!(old, new, ai) {
        put(t, "ai", new.ai.as_deref().map(Value::from));
    }
    if changed!(old, new, expand) {
        put(t, "expand", new.expand.as_deref().map(Value::from));
    }
    let keys: std::collections::BTreeSet<&String> =
        old.conditions.keys().chain(new.conditions.keys()).collect();
    for k in keys {
        let n = new.conditions.get(k);
        if old.conditions.get(k) != n {
            put(t, k, n.map(|j| condition_toml(k, j)));
        }
    }
}

fn write_output(t: &mut Table, old: &OutputDraft, new: &OutputDraft) {
    if changed!(old, new, dir) {
        put(t, "dir", Some(new.dir.as_str().into()));
    }
    if changed!(old, new, server_dir) {
        put(t, "server_dir", new.server_dir.as_deref().map(Value::from));
    }
    if changed!(old, new, content) {
        put(
            t,
            "content",
            (!new.content.is_empty()).then(|| steps_val(&new.content)),
        );
    }
    if changed!(old, new, rename) {
        put(
            t,
            "rename",
            (!new.rename.is_empty()).then(|| steps_val(&new.rename)),
        );
    }
}

fn filter_inline(f: &FilterDraft) -> InlineTable {
    let mut t = Table::new();
    write_filter(&mut t, &FilterDraft::default(), f);
    t.into_inline_table()
}

fn output_inline(o: &OutputDraft) -> InlineTable {
    let mut t = Table::new();
    write_output(&mut t, &OutputDraft::default(), o);
    t.into_inline_table()
}

/// How new sub-tables (`[[...filters]]`, `[[...outputs]]`) are separated:
/// as the file already does it.
#[derive(Debug, Clone)]
pub struct Style {
    pub sub_prefix: String,
}

impl Style {
    pub fn of(doc: &DocumentMut) -> Style {
        // The first filters/outputs header decides.
        let mut found = None;
        for (_, p) in super::order::render_order(doc) {
            if let Some(super::order::Seg::Idx(k, _) | super::order::Seg::Key(k)) = p.last()
                && matches!(k.as_str(), "filters" | "outputs")
                && p.len() > 1
            {
                let t = super::order::table(doc, &p);
                let prefix = t
                    .and_then(|t| t.decor().prefix())
                    .and_then(|r| r.as_str())
                    .unwrap_or("\n");
                let blank = prefix
                    .split_inclusive('\n')
                    .next()
                    .is_some_and(|l| l.trim().is_empty() && l.ends_with('\n'));
                found = Some(if blank { "\n" } else { "" }.to_string());
                break;
            }
        }
        Style {
            sub_prefix: found.unwrap_or_else(|| "\n".into()),
        }
    }
}

fn last_position(a: &ArrayOfTables) -> Option<isize> {
    a.iter().filter_map(Table::position).last()
}

/// Write the list `key` of `[[...]]` tables (or an inline array) from
/// `old` to `new`: changed elements in place, extra ones dropped from the
/// end, new ones appended after the last existing one (or at `pos`).
#[allow(clippy::too_many_arguments)]
fn write_list<T: PartialEq + Default>(
    t: &mut Table,
    key: &str,
    old: &[T],
    new: &[T],
    write_one: fn(&mut Table, &T, &T),
    inline: fn(&T) -> InlineTable,
    pos: Option<isize>,
    style: &Style,
) {
    if old == new {
        return;
    }
    let fresh = |x: &T, pos: Option<isize>| {
        let mut tb = Table::new();
        write_one(&mut tb, &T::default(), x);
        tb.set_position(pos);
        tb.decor_mut().set_prefix(style.sub_prefix.clone());
        tb
    };
    match t.get_mut(key) {
        Some(Item::ArrayOfTables(a)) => {
            for (i, (o, n)) in old.iter().zip(new).enumerate() {
                if o != n
                    && let Some(tb) = a.get_mut(i)
                {
                    write_one(tb, o, n);
                }
            }
            while a.len() > new.len() {
                a.remove(a.len() - 1);
            }
            let pos = last_position(a).or(pos);
            for x in new.iter().skip(old.len()) {
                a.push(fresh(x, pos));
            }
            if a.is_empty() {
                t.remove(key);
            }
        }
        Some(item) => {
            if new.is_empty() {
                t.remove(key);
            } else {
                let arr: Array = new.iter().map(|x| Value::InlineTable(inline(x))).collect();
                let decor = item.as_value().map(|v| v.decor().clone());
                let mut v = Value::Array(arr);
                if let Some(d) = decor {
                    *v.decor_mut() = d;
                }
                *item = Item::Value(v);
            }
        }
        None if !new.is_empty() => {
            let mut a = ArrayOfTables::new();
            for x in new {
                a.push(fresh(x, pos));
            }
            t.insert(key, Item::ArrayOfTables(a));
        }
        None => {}
    }
}

/// Write target `t` from `old` to `new`.
pub fn write_target(t: &mut Table, old: &TargetDraft, new: &TargetDraft, style: &Style) {
    if changed!(old, new, name) {
        put(t, "name", Some(new.name.as_str().into()));
    }
    if changed!(old, new, kind) {
        // `folders` is the default: not written.
        put(
            t,
            "kind",
            (!new.kind.is_folders()).then(|| Value::from(new.kind.as_str())),
        );
    }
    let own = t.position();
    write_list(
        t,
        "filters",
        &old.filters,
        &new.filters,
        write_filter,
        filter_inline,
        own,
        style,
    );
    // New outputs go after everything else of the target.
    let after = t
        .get("filters")
        .and_then(Item::as_array_of_tables)
        .and_then(last_position)
        .into_iter()
        .chain(own)
        .max();
    write_list(
        t,
        "outputs",
        &old.outputs,
        &new.outputs,
        write_output,
        output_inline,
        after,
        style,
    );
}

/// A new `[[targets]]` table for `d`.
pub fn new_target(d: &TargetDraft, style: &Style) -> Table {
    let mut t = Table::new();
    write_target(&mut t, &TargetDraft::default(), d, style);
    t
}

pub fn new_source(d: &SourceDraft) -> Table {
    let mut t = Table::new();
    write_source(&mut t, &SourceDraft::default(), d);
    t
}
