//! Filter conditions: pure value parsers shared by the routing conditions,
//! per-file conditions, and conditions on groups of files.

pub mod facts;
pub mod groups;
pub mod image_metrics;
pub mod range;

pub use facts::{Facts, Miss, check_file, facts_of};
pub use groups::{
    DupScope, Event, EventKey, Groups, NearIndex, Needs, Series, check_groups, compute, near_count,
    needs, same_groups,
};
pub use range::{DateRange, NumRange, TimeOfDay, Unit, parse_duration};
