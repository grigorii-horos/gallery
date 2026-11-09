//! Filter conditions in the editor: the typed values the UI sets
//! ([`ConditionValue`], the core's own condition structs) and the raw values
//! a draft shows (TOML as JSON), with the conversions between them and the
//! document.

use gallery_core::config::Place;
use gallery_core::filter::facts::{DateCond, DeviceCond, GeoCond, PropsCond};
use gallery_core::filter::groups::{DuplicateCond, EventCond, NearCond, SeriesCond};
use gallery_core::mask::MaskExpr;
use serde::{Deserialize, Serialize};
use toml_edit::{Array, DocumentMut, InlineTable, Item, Value};

/// The keys of a filter that hold a condition (besides `sources`, `match`,
/// `exclude`, `ai` and `expand`).
pub const CONDITION_KEYS: [&str; 9] = [
    "date",
    "geo",
    "device",
    "props",
    "path",
    "event",
    "near",
    "duplicate",
    "series",
];

/// One condition of a filter, as the core reads it: `{ "key": "near",
/// "value": { "sources": ["partner"], "time": "2h", "km": 1 } }`. The value has
/// the shape of the TOML value.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "key", content = "value", rename_all = "snake_case")]
pub enum ConditionValue {
    Date(Box<DateCond>),
    Geo(GeoCond),
    Device(DeviceCond),
    Props(Box<PropsCond>),
    Path(MaskExpr),
    Event(EventCond),
    Near(NearCond),
    Duplicate(DuplicateCond),
    Series(SeriesCond),
}

// The core's condition structs have no `PartialEq`: two conditions are equal
// when they are written the same.
impl PartialEq for ConditionValue {
    fn eq(&self, other: &Self) -> bool {
        serde_json::to_value(self).ok() == serde_json::to_value(other).ok()
    }
}

impl Eq for ConditionValue {}

/// `{ v = <x> }` as TOML, to read `x` back as a `toml_edit` value with the
/// keys in the order the struct writes them.
#[derive(Serialize)]
struct Wrap<'a, T: Serialize> {
    v: &'a T,
}

fn to_toml<T: Serialize>(x: &T) -> Result<Value, String> {
    let text = toml::to_string(&Wrap { v: x }).map_err(|e| e.to_string())?;
    let doc: DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| e.to_string())?;
    let mut v = match doc.get("v") {
        None => Value::InlineTable(InlineTable::new()),
        Some(Item::Value(v)) => v.clone(),
        Some(Item::Table(t)) => Value::InlineTable(t.clone().into_inline_table()),
        Some(_) => return Err("unexpected TOML for a condition".into()),
    };
    fmt_value(&mut v);
    Ok(v)
}

/// Normal spacing (`{ a = 1, b = [2, 3] }`) all the way down.
fn fmt_value(v: &mut Value) {
    match v {
        Value::Array(a) => {
            for x in a.iter_mut() {
                fmt_value(x);
            }
            a.fmt();
        }
        Value::InlineTable(t) => {
            for (_, x) in t.iter_mut() {
                fmt_value(x);
            }
            t.fmt();
        }
        _ => {}
    }
    v.decor_mut().clear();
}

impl ConditionValue {
    /// The filter key the condition is written under.
    pub fn key(&self) -> &'static str {
        match self {
            ConditionValue::Date(_) => "date",
            ConditionValue::Geo(_) => "geo",
            ConditionValue::Device(_) => "device",
            ConditionValue::Props(_) => "props",
            ConditionValue::Path(_) => "path",
            ConditionValue::Event(_) => "event",
            ConditionValue::Near(_) => "near",
            ConditionValue::Duplicate(_) => "duplicate",
            ConditionValue::Series(_) => "series",
        }
    }

    fn content(&self) -> Result<serde_json::Value, String> {
        let v = match self {
            ConditionValue::Date(c) => serde_json::to_value(c),
            ConditionValue::Geo(c) => serde_json::to_value(c),
            ConditionValue::Device(c) => serde_json::to_value(c),
            ConditionValue::Props(c) => serde_json::to_value(c),
            ConditionValue::Path(c) => serde_json::to_value(c),
            ConditionValue::Event(c) => serde_json::to_value(c),
            ConditionValue::Near(c) => serde_json::to_value(c),
            ConditionValue::Duplicate(c) => serde_json::to_value(c),
            ConditionValue::Series(c) => serde_json::to_value(c),
        };
        v.map_err(|e| e.to_string())
    }

    /// The raw value a draft holds for the condition.
    pub fn to_json(&self) -> Result<serde_json::Value, String> {
        self.content()
    }

    /// The condition `key` read from a raw draft value.
    pub fn from_json(key: &str, value: &serde_json::Value) -> Result<Self, String> {
        let tagged = serde_json::json!({ "key": key, "value": value });
        serde_json::from_value(tagged)
            .map_err(|e| gallery_core::t!("edit.bad_condition", key = key, error = e.to_string()))
    }

    /// The condition as an inline TOML value, keys in the core's order:
    /// `{ sources = ["partner"], time = "2h", km = 1.0 }`.
    pub fn to_toml(&self) -> Result<Value, String> {
        match self {
            ConditionValue::Date(c) => to_toml(c),
            ConditionValue::Geo(c) => to_toml(c),
            ConditionValue::Device(c) => to_toml(c),
            ConditionValue::Props(c) => to_toml(c),
            ConditionValue::Path(c) => to_toml(c),
            ConditionValue::Event(c) => to_toml(c),
            ConditionValue::Near(c) => to_toml(c),
            ConditionValue::Duplicate(c) => to_toml(c),
            ConditionValue::Series(c) => to_toml(c),
        }
    }
}

/// A place as an inline table: `{ lat = 47.0245, lon = 28.8323, km = 2.0 }`.
pub fn place_toml(p: &Place) -> Value {
    to_toml(p).unwrap_or_else(|_| {
        let mut t = InlineTable::new();
        t.insert("lat", p.lat.into());
        t.insert("lon", p.lon.into());
        t.insert("km", p.km.into());
        Value::InlineTable(t)
    })
}

// ---------- raw values ----------

/// A TOML item as JSON (dates as their text).
pub fn item_json(item: &Item) -> serde_json::Value {
    match item {
        Item::None => serde_json::Value::Null,
        Item::Value(v) => value_json(v),
        Item::Table(t) => serde_json::Value::Object(
            t.iter()
                .map(|(k, i)| (k.to_string(), item_json(i)))
                .collect(),
        ),
        Item::ArrayOfTables(a) => serde_json::Value::Array(
            a.iter()
                .map(|t| item_json(&Item::Table(t.clone())))
                .collect(),
        ),
    }
}

fn value_json(v: &Value) -> serde_json::Value {
    use serde_json::Value as J;
    match v {
        Value::String(s) => J::String(s.value().clone()),
        Value::Integer(i) => J::from(*i.value()),
        Value::Float(f) => serde_json::Number::from_f64(*f.value()).map_or(J::Null, J::Number),
        Value::Boolean(b) => J::Bool(*b.value()),
        Value::Datetime(d) => J::String(d.value().to_string()),
        Value::Array(a) => J::Array(a.iter().map(value_json).collect()),
        Value::InlineTable(t) => J::Object(
            t.iter()
                .map(|(k, x)| (k.to_string(), value_json(x)))
                .collect(),
        ),
    }
}

/// A raw JSON value as TOML, as it comes (for a value the core does not
/// accept: it is written so that the check can name it).
fn json_toml(j: &serde_json::Value) -> Value {
    use serde_json::Value as J;
    let mut v = match j {
        J::Null => Value::from(""),
        J::Bool(b) => Value::from(*b),
        J::Number(n) => match n.as_i64() {
            Some(i) => Value::from(i),
            None => Value::from(n.as_f64().unwrap_or(0.0)),
        },
        J::String(s) => Value::from(s.as_str()),
        J::Array(a) => Value::Array(a.iter().map(json_toml).collect::<Array>()),
        J::Object(o) => {
            let mut t = InlineTable::new();
            for (k, x) in o {
                t.insert(k, json_toml(x));
            }
            Value::InlineTable(t)
        }
    };
    fmt_value(&mut v);
    v
}

/// The TOML value for condition `key` with raw value `j`: in the core's key
/// order when the core accepts it, else as it comes.
pub fn condition_toml(key: &str, j: &serde_json::Value) -> Value {
    ConditionValue::from_json(key, j)
        .and_then(|c| c.to_toml())
        .unwrap_or_else(|_| json_toml(j))
}

/// Whether a raw mask value (a string or a list of strings) has `item`.
pub fn json_strings(j: &serde_json::Value) -> Vec<&str> {
    match j {
        serde_json::Value::String(s) => vec![s.as_str()],
        serde_json::Value::Array(a) => a.iter().filter_map(|x| x.as_str()).collect(),
        _ => Vec::new(),
    }
}

/// The mask values of a filter's conditions that may name `@masks`:
/// `path` and `device.make` / `model` / `software`.
pub fn mask_values_mut(
    conditions: &mut std::collections::BTreeMap<String, serde_json::Value>,
) -> Vec<&mut serde_json::Value> {
    let mut out = Vec::new();
    for (k, v) in conditions.iter_mut() {
        match k.as_str() {
            "path" => out.push(v),
            "device" => {
                if let serde_json::Value::Object(o) = v {
                    for (dk, dv) in o.iter_mut() {
                        if matches!(dk.as_str(), "make" | "model" | "software") {
                            out.push(dv);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn near_is_written_inline_in_the_core_order() {
        let c: ConditionValue = serde_json::from_value(serde_json::json!({
            "key": "near",
            "value": { "km": 1.0, "time": "2h", "sources": ["partner"] }
        }))
        .unwrap();
        assert_eq!(
            c.to_toml().unwrap().to_string(),
            r#"{ sources = ["partner"], time = "2h", km = 1.0 }"#
        );
    }

    #[test]
    fn geo_keeps_its_short_forms_and_not() {
        let c = ConditionValue::from_json("geo", &serde_json::json!("!@home")).unwrap();
        assert_eq!(c.to_toml().unwrap().to_string(), r#""!@home""#);
        let c = ConditionValue::from_json("geo", &serde_json::json!({ "has_gps": false })).unwrap();
        assert_eq!(c.to_toml().unwrap().to_string(), "{ has_gps = false }");
        let c = ConditionValue::from_json(
            "date",
            &serde_json::json!({ "time": "22:00..05:00", "not": true }),
        )
        .unwrap();
        assert_eq!(
            c.to_toml().unwrap().to_string(),
            r#"{ time = "22:00..05:00", not = true }"#
        );
    }

    #[test]
    fn bad_values_are_named() {
        let e =
            ConditionValue::from_json("date", &serde_json::json!({ "range": "soon" })).unwrap_err();
        assert!(e.contains("date"), "{e}");
        // As it comes when the core does not take it.
        assert_eq!(
            condition_toml("date", &serde_json::json!({ "range": "soon" })).to_string(),
            r#"{ range = "soon" }"#
        );
    }

    #[test]
    fn duplicate_with_defaults_is_an_empty_table() {
        let c = ConditionValue::from_json("duplicate", &serde_json::json!({})).unwrap();
        assert_eq!(c.to_toml().unwrap().to_string(), "{}");
    }

    #[test]
    fn toml_values_read_as_json() {
        let doc: DocumentMut = r#"x = { a = 1, b = 2.5, c = ["s"], d = true }"#.parse().unwrap();
        assert_eq!(
            item_json(doc.get("x").unwrap()),
            serde_json::json!({ "a": 1, "b": 2.5, "c": ["s"], "d": true })
        );
    }
}
