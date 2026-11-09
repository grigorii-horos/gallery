//! Filter conditions: pure value parsers shared by the routing conditions.

pub mod range;

pub use range::{DateRange, NumRange, TimeOfDay, Unit, parse_duration};
