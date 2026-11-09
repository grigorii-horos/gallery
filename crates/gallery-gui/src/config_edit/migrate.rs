//! The old three-stage shape (`targets.before_ai` / `ai` / `after_ai`)
//! rewritten in the document as the ordered `[[targets]]` list, the way
//! `Config::load` reads it: the stages concatenated in that order, an AI
//! target turned into one filter `{ sources, match / exclude of its
//! prefilter, ai = description }`. Tables keep their comments and their
//! keys' formatting; only the headers change.

use toml_edit::{ArrayOfTables, DocumentMut, Item, Key, Table, TableLike, Value};

use super::doc::Style;
use super::order::{Group, Shape, regroup};

const STAGES: [&str; 3] = ["before_ai", "ai", "after_ai"];

type R<T> = Result<T, String>;

fn not_understood(detail: String) -> String {
    gallery_core::t!("edit.not_understood", detail = detail)
}

/// The tables of stage `key` of the old `[targets]` table (an inline
/// array's tables become standard ones).
fn stage_tables(targets: &dyn TableLike, key: &str) -> R<Vec<Table>> {
    match targets.get(key) {
        None => Ok(Vec::new()),
        Some(Item::ArrayOfTables(a)) => Ok(a.iter().cloned().collect()),
        Some(Item::Value(Value::Array(a))) => a
            .iter()
            .map(|v| {
                v.as_inline_table()
                    .map(|t| t.clone().into_table())
                    .ok_or_else(|| {
                        not_understood(gallery_core::t!(
                            "edit.expected",
                            path = format!("targets.{key}"),
                            what = gallery_core::t!("edit.what.tables")
                        ))
                    })
            })
            .collect(),
        Some(_) => Err(not_understood(gallery_core::t!(
            "edit.expected",
            path = format!("targets.{key}"),
            what = gallery_core::t!("edit.what.tables")
        ))),
    }
}

/// Move `key` of `from` into `to` as `new_key`, with its formatting and the
/// comments above it.
fn move_key(from: &mut Table, key: &str, to: &mut Table, new_key: &str) {
    if let Some((k, item)) = from.remove_entry(key) {
        let nk = Key::new(new_key).with_leaf_decor(k.leaf_decor().clone());
        to.insert_formatted(&nk, item);
    }
}

/// An old AI target as a target with one filter.
fn ai_target(mut t: Table, style: &Style) -> Table {
    let mut f = Table::new();
    move_key(&mut t, "sources", &mut f, "sources");
    match t.remove("prefilter") {
        Some(Item::Table(p)) => {
            // The filter's header goes where the prefilter's was.
            f.set_position(p.position());
            *f.decor_mut() = p.decor().clone();
            for (k, item) in p.iter() {
                let key = p.key(k).cloned().unwrap_or_else(|| Key::new(k));
                f.insert_formatted(&key, item.clone());
            }
        }
        Some(Item::Value(Value::InlineTable(p))) => {
            f.set_position(t.position());
            f.decor_mut().set_prefix(style.sub_prefix.clone());
            for (k, v) in p.iter() {
                let mut v = v.clone();
                v.decor_mut().clear();
                f.insert(k, Item::Value(v));
            }
        }
        _ => {
            f.set_position(t.position());
            f.decor_mut().set_prefix(style.sub_prefix.clone());
        }
    }
    move_key(&mut t, "description", &mut f, "ai");
    let mut filters = ArrayOfTables::new();
    filters.push(f);
    t.insert("filters", Item::ArrayOfTables(filters));
    t
}

/// Whether the document's `targets` is the old three-stage table: some
/// stage lists a target (as in the core, stages that are all empty are not).
pub fn is_old_shape(doc: &DocumentMut) -> bool {
    doc.get("targets")
        .and_then(Item::as_table_like)
        .is_some_and(|t| {
            t.iter().any(|(k, v)| {
                STAGES.contains(&k)
                    && match v {
                        Item::ArrayOfTables(a) => !a.is_empty(),
                        Item::Value(Value::Array(a)) => !a.is_empty(),
                        // Not a list: the shape is old, the edit reports it.
                        _ => true,
                    }
            })
        })
}

/// Rewrite an old-shape `targets` table as the `[[targets]]` list. `false`
/// (nothing changed) when the document is not in the old shape.
pub fn to_ordered(doc: &mut DocumentMut) -> R<bool> {
    let Some(old) = doc.get("targets").and_then(Item::as_table_like) else {
        return Ok(false);
    };
    if let Some((k, _)) = old.iter().find(|(k, _)| !STAGES.contains(k)) {
        return Err(not_understood(format!("targets.{k}")));
    }
    if !is_old_shape(doc) {
        // `[targets]` naming no stage target is not the old shape; an empty list.
        doc.remove("targets");
        return Ok(false);
    }
    let style = Style::of(doc);
    let mut list: Vec<Table> = Vec::new();
    for key in STAGES {
        for t in stage_tables(old, key)? {
            list.push(if key == "ai" { ai_target(t, &style) } else { t });
        }
    }
    // The comments above an explicit `[targets]` header go to the first target.
    let heading = doc
        .get("targets")
        .and_then(Item::as_table)
        .filter(|t| !t.is_implicit())
        .and_then(|t| t.decor().prefix())
        .and_then(|p| p.as_str())
        .map(str::to_string);
    if let (Some(h), Some(first)) = (heading, list.first_mut()) {
        let own = first
            .decor()
            .prefix()
            .and_then(|p| p.as_str())
            .unwrap_or("")
            .to_string();
        first
            .decor_mut()
            .set_prefix(format!("{h}{}", own.trim_start_matches('\n')));
    }
    let mut aot = ArrayOfTables::new();
    for t in list {
        aot.push(t);
    }
    doc.insert("targets", Item::ArrayOfTables(aot));
    // The blocks in list order (the file may have had the stages in another).
    regroup(doc, Group::Targets, Shape::Slots, |_| ())?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> DocumentMut {
        s.parse().unwrap()
    }

    /// Stage lists that are all empty are not the old shape (as in the
    /// core): no `.bak` is held back for them and nothing is rewritten.
    #[test]
    fn all_empty_stages_are_not_the_old_shape() {
        let empty = "[targets]\nbefore_ai = []\nai = []\n";
        assert!(!is_old_shape(&parse(empty)));
        let mut d = parse(empty);
        assert!(!to_ordered(&mut d).unwrap());
        assert!(d.get("targets").is_none());
        assert!(is_old_shape(&parse("[[targets.after_ai]]\nname = \"A\"\n")));
    }
}
