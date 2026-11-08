//! Where tables land in the file. `toml_edit` writes tables in the order of
//! their document positions (a table without one follows the table before
//! it in tree order), so adding, removing and moving `[[targets.*]]` blocks
//! means giving their tables positions in the wanted order.

use toml_edit::{ArrayOfTables, DocumentMut, Item, Table};

/// One step from a table to a sub-table: `key` (a `[key]` table) or the
/// `index`th `[[key]]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seg {
    Key(String),
    Idx(String, usize),
}

pub type TablePath = Vec<Seg>;

fn collect(t: &Table, path: &mut TablePath, out: &mut Vec<(Option<isize>, TablePath)>) {
    if !t.is_dotted() {
        out.push((t.position(), path.clone()));
    }
    for (k, item) in t.iter() {
        match item {
            Item::Table(st) => {
                path.push(Seg::Key(k.to_string()));
                collect(st, path, out);
                path.pop();
            }
            Item::ArrayOfTables(a) => {
                for (i, st) in a.iter().enumerate() {
                    path.push(Seg::Idx(k.to_string(), i));
                    collect(st, path, out);
                    path.pop();
                }
            }
            _ => {}
        }
    }
}

/// Every table but the root with its effective position, in tree order: a
/// table without a position takes the one of the table before it.
fn tree(doc: &DocumentMut) -> Vec<(isize, TablePath)> {
    let mut raw = Vec::new();
    collect(doc.as_table(), &mut Vec::new(), &mut raw);
    let mut last = 0;
    raw.into_iter()
        .map(|(p, path)| {
            let p = p.unwrap_or(last);
            last = p;
            (p, path)
        })
        .filter(|(_, path)| !path.is_empty())
        .collect()
}

/// The tables in the order they are written to the file (as `toml_edit`'s
/// `Display` sorts them).
pub fn render_order(doc: &DocumentMut) -> Vec<(isize, TablePath)> {
    let mut t = tree(doc);
    t.sort_by_key(|(p, _)| *p);
    t
}

pub fn table<'a>(doc: &'a DocumentMut, path: &[Seg]) -> Option<&'a Table> {
    let mut t = doc.as_table();
    for s in path {
        t = match s {
            Seg::Key(k) => t.get(k)?.as_table()?,
            Seg::Idx(k, i) => t.get(k)?.as_array_of_tables()?.get(*i)?,
        };
    }
    Some(t)
}

pub fn table_mut<'a>(doc: &'a mut DocumentMut, path: &[Seg]) -> Option<&'a mut Table> {
    let mut t = doc.as_table_mut();
    for s in path {
        t = match s {
            Seg::Key(k) => t.get_mut(k)?.as_table_mut()?,
            Seg::Idx(k, i) => t.get_mut(k)?.as_array_of_tables_mut()?.get_mut(*i)?,
        };
    }
    Some(t)
}

/// Positions spaced by this much: room to slot a new table after another.
const STEP: isize = 16;

fn assign(doc: &mut DocumentMut, order: &[TablePath]) {
    for (k, p) in order.iter().enumerate() {
        if let Some(t) = table_mut(doc, p) {
            t.set_position(Some(k as isize * STEP));
        }
    }
}

/// Give every table an explicit, distinct position in its current written
/// order. The text does not change.
pub fn renumber(doc: &mut DocumentMut) {
    let order: Vec<TablePath> = render_order(doc).into_iter().map(|(_, p)| p).collect();
    assign(doc, &order);
}

/// A group of `[[...]]` blocks edited as a unit: `[[sources]]` or the
/// targets of one stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Sources,
    Stage(super::model::Stage),
}

impl Group {
    /// Path of the array's parent table and the array's key.
    fn parent_and_key(self) -> (TablePath, &'static str) {
        match self {
            Group::Sources => (Vec::new(), "sources"),
            Group::Stage(s) => (vec![Seg::Key("targets".into())], s.key()),
        }
    }

    /// Whether table `path` belongs to element `index` of this group
    /// (`None`: to any element).
    fn owns(self, path: &[Seg], index: Option<usize>) -> bool {
        let (parent, key) = self.parent_and_key();
        path.len() > parent.len()
            && path[..parent.len()] == parent[..]
            && matches!(&path[parent.len()], Seg::Idx(k, i) if k == key && index.is_none_or(|x| x == *i))
    }

    fn rank(self) -> usize {
        match self {
            Group::Sources => 0,
            Group::Stage(s) => 1 + s as usize,
        }
    }
}

/// Check that group `g` can be edited as `[[...]]` blocks: its array is
/// absent or an array of tables, and `targets` is a normal table.
fn check_group(doc: &DocumentMut, g: Group) -> Result<(), String> {
    let (parent, key) = g.parent_and_key();
    let name = if parent.is_empty() {
        key.to_string()
    } else {
        format!("targets.{key}")
    };
    if !parent.is_empty() {
        match doc.get("targets") {
            None => {}
            Some(Item::Table(_)) => {}
            Some(_) => {
                return Err("[targets] is not written as a table — edit it in the file".into());
            }
        }
    }
    match table(doc, &parent).and_then(|p| p.get(key)) {
        None => Ok(()),
        Some(Item::ArrayOfTables(_)) => Ok(()),
        Some(_) => Err(format!(
            "{name} is not written as [[{name}]] — edit it in the file"
        )),
    }
}

/// The group's array (made when absent); `check_group` has passed.
fn aot_mut(doc: &mut DocumentMut, g: Group) -> &mut ArrayOfTables {
    let (parent, key) = g.parent_and_key();
    if !parent.is_empty() && table(doc, &parent).is_none() {
        let mut t = Table::new();
        t.set_implicit(true);
        doc.insert("targets", Item::Table(t));
    }
    let p = table_mut(doc, &parent).expect("parent made above");
    if !p.get(key).is_some_and(Item::is_array_of_tables) {
        p.insert(key, Item::ArrayOfTables(ArrayOfTables::new()));
    }
    p.get_mut(key)
        .and_then(Item::as_array_of_tables_mut)
        .expect("made above")
}

/// Split a header's leading whitespace and comments into the part that
/// belongs to the place (up to the last blank line: a section heading) and
/// the comment right above the header, which belongs to the block.
pub fn split_prefix(prefix: &str) -> (String, String) {
    let lines: Vec<&str> = prefix.split_inclusive('\n').collect();
    match lines
        .iter()
        .rposition(|l| l.trim().is_empty() && l.ends_with('\n'))
    {
        Some(i) => (lines[..=i].concat(), lines[i + 1..].concat()),
        None => (String::new(), prefix.to_string()),
    }
}

fn prefix_of(t: &Table) -> String {
    t.decor()
        .prefix()
        .and_then(|r| r.as_str())
        .unwrap_or("\n")
        .to_string()
}

/// What `regroup`'s change does to the slots (and their headings).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// Blocks are added or reordered: a heading belongs to its slot.
    Slots,
    /// Block `i` is removed.
    Remove(usize),
}

/// Change the blocks of group `g` with `mutate` (insert, remove, reorder
/// elements of its array) and lay them out again: the group's blocks are
/// written together, in array order, where the group began (an empty group
/// goes after the groups before it). Each slot keeps its section heading
/// (the comments above the last blank line); a block keeps the comment right
/// above its header.
///
/// With `Shape::Remove(i)` the removed block's section heading is not lost:
/// it goes in front of the next block's heading (or, for the last block, in
/// front of whatever follows the group); later blocks keep their headings.
///
/// An error (nothing changed) when the group is not written as `[[...]]`
/// blocks: it is never replaced.
pub fn regroup<T>(
    doc: &mut DocumentMut,
    g: Group,
    shape: Shape,
    mutate: impl FnOnce(&mut ArrayOfTables) -> T,
) -> Result<T, String> {
    check_group(doc, g)?;
    renumber(doc);
    let before = render_order(doc);
    let old: Vec<(String, String)> = aot_mut(doc, g)
        .iter()
        .map(|t| split_prefix(&prefix_of(t)))
        .collect();
    let anchor = before
        .iter()
        .filter(|(_, p)| g.owns(p, None))
        .map(|(pos, _)| *pos)
        .min();

    let out = mutate(aot_mut(doc, g));

    // Order of the other tables, then the group's blocks in array order.
    let now = tree(doc);
    let mut others: Vec<(isize, TablePath)> = now
        .iter()
        .filter(|(_, p)| !g.owns(p, None))
        .cloned()
        .collect();
    others.sort_by_key(|(p, _)| *p);
    let n = aot_mut(doc, g).len();
    let mut blocks = Vec::new();
    for i in 0..n {
        let mut b: Vec<(isize, TablePath)> = now
            .iter()
            .filter(|(_, p)| g.owns(p, Some(i)))
            .cloned()
            .collect();
        // A new block's tables have no positions yet: keep their tree order.
        let head = b.first().map(|(p, _)| *p).unwrap_or(0);
        if table(doc, &b[0].1).and_then(Table::position).is_none() {
            for (p, _) in &mut b {
                *p = head;
            }
        }
        b.sort_by_key(|(p, _)| *p);
        blocks.extend(b.into_iter().map(|(_, p)| p));
    }
    let at = match anchor {
        Some(a) => others.iter().position(|(p, _)| *p > a),
        None => {
            // After the last table of an earlier group, else before the
            // first of a later one, else at the end.
            let rank_of = |p: &TablePath| {
                [Group::Sources]
                    .into_iter()
                    .chain(super::model::Stage::ALL.map(Group::Stage))
                    .find(|x| x.owns(p, None))
                    .map(Group::rank)
            };
            let earlier = others
                .iter()
                .rposition(|(_, p)| rank_of(p).is_some_and(|r| r < g.rank()));
            match earlier {
                Some(i) => Some(i + 1),
                None => others
                    .iter()
                    .position(|(_, p)| rank_of(p).is_some_and(|r| r > g.rank()))
                    .or(if g == Group::Sources { Some(0) } else { None }),
            }
        }
    }
    .unwrap_or(others.len());
    let mut order: Vec<TablePath> = others.into_iter().map(|(_, p)| p).collect();
    order.splice(at..at, blocks);
    assign(doc, &order);

    // Section headings: per slot, or merged on a removal.
    let mut slots: Vec<String> = old.iter().map(|(h, _)| h.clone()).collect();
    let mut orphan = String::new();
    if let Shape::Remove(i) = shape
        && i < slots.len()
    {
        let gone = slots.remove(i);
        match slots.get_mut(i) {
            Some(next) if next.trim().is_empty() => *next = gone,
            Some(next) => *next = format!("{gone}{}", next.trim_start_matches('\n')),
            None if !gone.trim().is_empty() => orphan = gone,
            None => {}
        }
    }
    let a = aot_mut(doc, g);
    for i in 0..a.len() {
        let t = a.get_mut(i).expect("index in range");
        let (_, own) = split_prefix(&prefix_of(t));
        let place = slots.get(i).cloned().unwrap_or_else(|| "\n".to_string());
        let new = format!("{place}{own}");
        if new != prefix_of(t) || t.decor().prefix().is_none() {
            t.decor_mut().set_prefix(new);
        }
    }
    if !orphan.is_empty() {
        let after = order.get(at + n).cloned();
        match after.and_then(|p| table_mut(doc, &p)) {
            Some(t) => {
                let cur = prefix_of(t);
                let new = if cur.trim().is_empty() {
                    orphan
                } else {
                    format!("{orphan}{}", cur.trim_start_matches('\n'))
                };
                t.decor_mut().set_prefix(new);
            }
            None => {
                // Nothing follows: the heading goes to the end of the file.
                let tail = doc
                    .trailing()
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_default();
                let new = if tail.trim().is_empty() {
                    orphan
                } else {
                    format!("{orphan}{tail}")
                };
                doc.set_trailing(new);
            }
        }
    }
    if aot_mut(doc, g).is_empty() {
        let (parent, key) = g.parent_and_key();
        if let Some(p) = table_mut(doc, &parent) {
            p.remove(key);
        }
    }
    Ok(out)
}
