//! Typed edit operations on the document.

use gallery_core::config::Place;
use serde::{Deserialize, Serialize};
use toml_edit::{DocumentMut, Item, Table, Value};

use super::cond::{CONDITION_KEYS, ConditionValue, place_toml};
use super::doc::{Style, new_source, new_target, put, read_draft, write_source, write_target};
use super::model::{
    ContentPresetDraft, MaskDraft, NamePresetDraft, PresetKind, SourceDraft, TargetDraft,
    TargetKind,
};
use super::order::{Group, Seg, Shape, regroup, render_order, renumber};
use super::presets;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum EditOp {
    AddSource {
        source: SourceDraft,
    },
    /// Replace source `id` (a new id is also changed in every target that
    /// names it).
    UpdateSource {
        id: String,
        source: SourceDraft,
    },
    /// Remove source `id` and drop it from every target's source list.
    RemoveSource {
        id: String,
    },
    /// Append a target of `kind` to the end of the list: `target` (its
    /// own `kind` is replaced), or an empty one.
    AddTarget {
        kind: TargetKind,
        #[serde(default)]
        target: TargetDraft,
    },
    /// Replace target `index` (the list's position).
    UpdateTarget {
        index: usize,
        target: TargetDraft,
    },
    RemoveTarget {
        index: usize,
    },
    /// Move target `from` so that it ends up at position `to`.
    MoveTarget {
        from: usize,
        to: usize,
    },
    /// Change what target `name` does with the files it takes.
    SetTargetKind {
        name: String,
        kind: TargetKind,
    },
    /// Set (add or replace) a condition of filter `filter` of target
    /// `target` (by name).
    SetCondition {
        target: String,
        filter: usize,
        condition: ConditionValue,
    },
    /// Remove condition `key` (also `ai` or `expand`) of a filter.
    RemoveCondition {
        target: String,
        filter: usize,
        key: String,
    },
    /// Add or replace place `name` of `[places]`.
    SetPlace {
        name: String,
        place: Place,
    },
    RemovePlace {
        name: String,
    },
    AddNamePreset {
        preset: NamePresetDraft,
    },
    /// Replace the fields of name preset `name` (its name stays: see
    /// `RenamePreset`).
    UpdateNamePreset {
        name: String,
        preset: NamePresetDraft,
    },
    /// Refused while a target or mask uses it.
    RemoveNamePreset {
        name: String,
    },
    AddContentPreset {
        preset: ContentPresetDraft,
    },
    UpdateContentPreset {
        name: String,
        preset: ContentPresetDraft,
    },
    RemoveContentPreset {
        name: String,
    },
    AddMask {
        mask: MaskDraft,
    },
    UpdateMask {
        name: String,
        mask: MaskDraft,
    },
    RemoveMask {
        name: String,
    },
    /// Rename a user preset or mask in place; with `update_refs`, every
    /// pipe step, filter and mask that names it follows.
    RenamePreset {
        kind: PresetKind,
        from: String,
        to: String,
        update_refs: bool,
    },
    /// Add a copy of built-in `from` named `to` (a free, non-built-in name).
    CopyBuiltin {
        kind: PresetKind,
        from: String,
        to: String,
    },
}

type R<T> = Result<T, String>;

fn source_index(doc: &DocumentMut, id: &str) -> R<usize> {
    read_draft(doc)?
        .sources
        .iter()
        .position(|s| s.id == id)
        .ok_or_else(|| gallery_core::t!("edit.no_source", id = id))
}

fn check_index(doc: &DocumentMut, index: usize) -> R<()> {
    let n = read_draft(doc)?.targets.len();
    if index < n {
        Ok(())
    } else {
        Err(gallery_core::t!("edit.no_target_at", n = index + 1))
    }
}

fn target_index(doc: &DocumentMut, name: &str) -> R<usize> {
    read_draft(doc)?
        .targets
        .iter()
        .position(|t| t.name == name)
        .ok_or_else(|| gallery_core::t!("edit.no_target_named", name = name))
}

/// Change target `i` of the document with `f` on its draft.
fn update_target(
    doc: &mut DocumentMut,
    i: usize,
    f: impl FnOnce(&mut TargetDraft) -> R<()>,
) -> R<()> {
    check_index(doc, i)?;
    let old = read_draft(doc)?.targets[i].clone();
    let mut new = old.clone();
    f(&mut new)?;
    if new != old {
        let style = Style::of(doc);
        renumber(doc);
        let t = target_table(doc, i)?;
        write_target(t, &old, &new, &style);
        renumber(doc);
    }
    Ok(())
}

/// Change filter `filter` of target `name` with `f`.
fn update_filter(
    doc: &mut DocumentMut,
    name: &str,
    filter: usize,
    f: impl FnOnce(&mut super::model::FilterDraft) -> R<()>,
) -> R<()> {
    let i = target_index(doc, name)?;
    update_target(doc, i, |t| {
        let fl = t
            .filters
            .get_mut(filter)
            .ok_or_else(|| gallery_core::t!("edit.no_filter_at", name = name, n = filter + 1))?;
        f(fl)
    })
}

/// Rewrite every target's source list with `f` (only targets it changes).
fn map_target_sources(doc: &mut DocumentMut, f: impl Fn(&[String]) -> Vec<String>) -> R<()> {
    let draft = read_draft(doc)?;
    let style = Style::of(doc);
    for (i, t) in draft.targets.iter().enumerate() {
        let mut new = t.clone();
        for fl in &mut new.filters {
            fl.sources = f(&fl.sources);
            for key in ["near", "event"] {
                if let Some(serde_json::Value::Object(o)) = fl.conditions.get_mut(key)
                    && let Some(serde_json::Value::Array(a)) = o.get_mut("sources")
                {
                    let ids: Vec<String> = a
                        .iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect();
                    *a = f(&ids).into_iter().map(serde_json::Value::from).collect();
                }
            }
        }
        if new != *t {
            renumber(doc);
            let tb = target_table(doc, i)?;
            write_target(tb, t, &new, &style);
        }
    }
    Ok(())
}

/// The table of target `i`.
pub(super) fn target_table(doc: &mut DocumentMut, i: usize) -> R<&mut Table> {
    doc.get_mut("targets")
        .and_then(Item::as_array_of_tables_mut)
        .and_then(|a| a.get_mut(i))
        .ok_or_else(|| gallery_core::t!("edit.targets_not_list"))
}

/// `[places]` (made after the sources, else before the first target, when
/// absent).
fn places_mut(doc: &mut DocumentMut) -> R<&mut Table> {
    match doc.get("places") {
        None => {
            renumber(doc);
            let order = render_order(doc);
            let first_of = |k: &str| {
                order
                    .iter()
                    .filter(
                        |(_, p)| matches!(p.first(), Some(Seg::Idx(x, _) | Seg::Key(x)) if x == k),
                    )
                    .map(|(pos, _)| *pos)
                    .collect::<Vec<_>>()
            };
            let pos = match (first_of("sources").last(), first_of("targets").first()) {
                (Some(s), _) => s + 1,
                (None, Some(t)) => t - 1,
                (None, None) => order.last().map_or(1, |(p, _)| p + 1),
            };
            let mut t = Table::new();
            t.set_position(Some(pos));
            t.decor_mut().set_prefix("\n");
            doc.insert("places", Item::Table(t));
            // Any table sharing the position: keep the file order.
            renumber(doc);
        }
        Some(Item::Table(_)) => {}
        Some(_) => {
            return Err(gallery_core::t!("edit.group_not_table", key = "places"));
        }
    }
    Ok(doc
        .get_mut("places")
        .and_then(Item::as_table_mut)
        .expect("made above"))
}

fn set_place(doc: &mut DocumentMut, name: &str, p: &Place) -> R<()> {
    if name.trim().is_empty() || name.trim() != name {
        return Err(gallery_core::t!("edit.place_name", name = name));
    }
    let places = places_mut(doc)?;
    match places.get_mut(name) {
        Some(Item::Table(t)) => {
            for (k, v) in [("lat", p.lat), ("lon", p.lon), ("km", p.km)] {
                put(t, k, Some(Value::from(v)));
            }
        }
        _ => put(places, name, Some(place_toml(p))),
    }
    Ok(())
}

fn remove_place(doc: &mut DocumentMut, name: &str) -> R<()> {
    let gone = doc
        .get_mut("places")
        .and_then(Item::as_table_like_mut)
        .and_then(|t| t.remove(name));
    if gone.is_none() {
        return Err(gallery_core::t!("edit.no_place", name = name));
    }
    if doc
        .get("places")
        .and_then(Item::as_table_like)
        .is_some_and(|t| t.is_empty())
    {
        doc.remove("places");
    }
    Ok(())
}

/// Set condition `c` in a filter draft.
fn set_condition(f: &mut super::model::FilterDraft, c: &ConditionValue) -> R<()> {
    f.conditions.insert(c.key().to_string(), c.to_json()?);
    Ok(())
}

fn remove_condition(f: &mut super::model::FilterDraft, key: &str) -> R<()> {
    let had = match key {
        "ai" => f.ai.take().is_some(),
        "expand" => f.expand.take().is_some(),
        k if CONDITION_KEYS.contains(&k) => f.conditions.remove(k).is_some(),
        _ => return Err(gallery_core::t!("edit.unknown_condition", key = key)),
    };
    if had {
        Ok(())
    } else {
        Err(gallery_core::t!("edit.no_condition", key = key))
    }
}

pub fn apply(doc: &mut DocumentMut, op: &EditOp) -> R<()> {
    let style = Style::of(doc);
    match op {
        EditOp::AddSource { source } => {
            let t = new_source(source);
            regroup(doc, Group::Sources, Shape::Slots, |a| a.push(t))?;
        }
        EditOp::UpdateSource { id, source } => {
            let i = source_index(doc, id)?;
            let old = read_draft(doc)?.sources[i].clone();
            let t = doc
                .get_mut("sources")
                .and_then(|s| s.as_array_of_tables_mut())
                .and_then(|a| a.get_mut(i))
                .ok_or_else(|| gallery_core::t!("edit.sources_not_table"))?;
            write_source(t, &old, source);
            if source.id != *id {
                let (from, to) = (id.clone(), source.id.clone());
                map_target_sources(doc, |l| {
                    l.iter()
                        .map(|s| if *s == from { to.clone() } else { s.clone() })
                        .collect()
                })?;
            }
        }
        EditOp::RemoveSource { id } => {
            let i = source_index(doc, id)?;
            regroup(doc, Group::Sources, Shape::Remove(i), |a| a.remove(i))?;
            map_target_sources(doc, |l| l.iter().filter(|s| *s != id).cloned().collect())?;
        }
        EditOp::AddTarget { kind, target } => {
            let target = TargetDraft {
                kind: *kind,
                ..target.clone()
            };
            let t = new_target(&target, &style);
            regroup(doc, Group::Targets, Shape::Slots, |a| a.push(t))?;
        }
        EditOp::UpdateTarget { index, target } => {
            check_index(doc, *index)?;
            target_table(doc, *index)?;
            update_target(doc, *index, |t| {
                *t = target.clone();
                Ok(())
            })?;
        }
        EditOp::RemoveTarget { index } => {
            check_index(doc, *index)?;
            target_table(doc, *index)?;
            regroup(doc, Group::Targets, Shape::Remove(*index), |a| {
                a.remove(*index)
            })?;
        }
        EditOp::MoveTarget { from, to } => {
            check_index(doc, *from)?;
            target_table(doc, *from)?;
            let n = read_draft(doc)?.targets.len();
            if *to >= n {
                return Err(gallery_core::t!("edit.no_position", n = to + 1));
            }
            regroup(doc, Group::Targets, Shape::Slots, |a| {
                let t = a.remove(*from);
                a.insert(*to, t);
            })?;
        }
        EditOp::SetTargetKind { name, kind } => {
            let i = target_index(doc, name)?;
            target_table(doc, i)?;
            update_target(doc, i, |t| {
                t.kind = *kind;
                Ok(())
            })?;
        }
        EditOp::SetCondition {
            target,
            filter,
            condition,
        } => update_filter(doc, target, *filter, |f| set_condition(f, condition))?,
        EditOp::RemoveCondition {
            target,
            filter,
            key,
        } => update_filter(doc, target, *filter, |f| remove_condition(f, key))?,
        EditOp::SetPlace { name, place } => set_place(doc, name, place)?,
        EditOp::RemovePlace { name } => remove_place(doc, name)?,
        EditOp::AddNamePreset { preset } => presets::add_name(doc, preset)?,
        EditOp::UpdateNamePreset { name, preset } => presets::update_name(doc, name, preset)?,
        EditOp::RemoveNamePreset { name } => presets::remove(doc, PresetKind::Name, name)?,
        EditOp::AddContentPreset { preset } => presets::add_content(doc, preset)?,
        EditOp::UpdateContentPreset { name, preset } => presets::update_content(doc, name, preset)?,
        EditOp::RemoveContentPreset { name } => presets::remove(doc, PresetKind::Content, name)?,
        EditOp::AddMask { mask } => presets::add_mask_op(doc, mask)?,
        EditOp::UpdateMask { name, mask } => presets::update_mask(doc, name, mask)?,
        EditOp::RemoveMask { name } => presets::remove(doc, PresetKind::Mask, name)?,
        EditOp::RenamePreset {
            kind,
            from,
            to,
            update_refs,
        } => presets::rename(doc, *kind, from, to, *update_refs)?,
        EditOp::CopyBuiltin { kind, from, to } => presets::copy_builtin(doc, *kind, from, to)?,
    }
    Ok(())
}
