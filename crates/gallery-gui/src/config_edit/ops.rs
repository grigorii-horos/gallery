//! Typed edit operations on the document.

use serde::{Deserialize, Serialize};
use toml_edit::DocumentMut;

use super::doc::{Style, new_source, new_target, read_draft, write_source, write_target};
use super::model::{
    ContentPresetDraft, MaskDraft, NamePresetDraft, PresetKind, SourceDraft, Stage, TargetDraft,
};
use super::order::{Group, Shape, regroup, renumber};
use super::presets;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Append a target to `stage`.
    AddTarget {
        stage: Stage,
        target: TargetDraft,
    },
    UpdateTarget {
        stage: Stage,
        index: usize,
        target: TargetDraft,
    },
    RemoveTarget {
        stage: Stage,
        index: usize,
    },
    /// Move a target within its stage, or between the two mask stages.
    MoveTarget {
        from: Stage,
        index: usize,
        to: Stage,
        to_index: usize,
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

fn check_index(doc: &DocumentMut, stage: Stage, index: usize) -> R<()> {
    let n = read_draft(doc)?.targets.stage(stage).len();
    if index < n {
        Ok(())
    } else {
        Err(gallery_core::t!(
            "edit.no_target_at",
            stage = stage.key(),
            n = index + 1
        ))
    }
}

/// Rewrite every target's source list with `f` (only targets it changes).
fn map_target_sources(doc: &mut DocumentMut, f: impl Fn(&[String]) -> Vec<String>) -> R<()> {
    let draft = read_draft(doc)?;
    let style = Style::of(doc);
    for (stage, i, t) in draft.targets.all() {
        let mut new = t.clone();
        for fl in &mut new.filters {
            fl.sources = f(&fl.sources);
        }
        new.sources = f(&new.sources);
        if new != *t {
            renumber(doc);
            let tb = stage_table(doc, stage, i)?;
            write_target(tb, stage, t, &new, &style);
        }
    }
    Ok(())
}

pub(super) fn stage_table(
    doc: &mut DocumentMut,
    stage: Stage,
    i: usize,
) -> R<&mut toml_edit::Table> {
    doc.get_mut("targets")
        .and_then(|t| t.get_mut(stage.key()))
        .and_then(|a| a.as_array_of_tables_mut())
        .and_then(|a| a.get_mut(i))
        .ok_or_else(|| gallery_core::t!("edit.target_not_table", stage = stage.key(), n = i + 1))
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
        EditOp::AddTarget { stage, target } => {
            let t = new_target(*stage, target, &style);
            regroup(doc, Group::Stage(*stage), Shape::Slots, |a| a.push(t))?;
        }
        EditOp::UpdateTarget {
            stage,
            index,
            target,
        } => {
            check_index(doc, *stage, *index)?;
            let old = read_draft(doc)?.targets.stage(*stage)[*index].clone();
            renumber(doc);
            let t = stage_table(doc, *stage, *index)?;
            write_target(t, *stage, &old, target, &style);
            renumber(doc);
        }
        EditOp::RemoveTarget { stage, index } => {
            check_index(doc, *stage, *index)?;
            stage_table(doc, *stage, *index)?;
            regroup(doc, Group::Stage(*stage), Shape::Remove(*index), |a| {
                a.remove(*index)
            })?;
        }
        EditOp::MoveTarget {
            from,
            index,
            to,
            to_index,
        } => {
            check_index(doc, *from, *index)?;
            stage_table(doc, *from, *index)?;
            if from.is_mask() != to.is_mask() {
                return Err(gallery_core::t!("edit.move_across_kinds"));
            }
            let len_to = read_draft(doc)?.targets.stage(*to).len();
            if from == to {
                if *to_index >= len_to {
                    return Err(gallery_core::t!("edit.no_position", n = to_index + 1));
                }
                regroup(doc, Group::Stage(*from), Shape::Slots, |a| {
                    let t = a.remove(*index);
                    a.insert(*to_index, t);
                })?;
            } else {
                if *to_index > len_to {
                    return Err(gallery_core::t!("edit.no_position", n = to_index + 1));
                }
                let t = regroup(doc, Group::Stage(*from), Shape::Slots, |a| a.remove(*index))?;
                regroup(doc, Group::Stage(*to), Shape::Slots, |a| {
                    a.insert(*to_index, t)
                })?;
            }
        }
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
