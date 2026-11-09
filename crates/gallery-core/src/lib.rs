//! Gallery core library: configuration, planning and media processing.

pub mod ai;
pub mod apply;
pub mod config;
pub mod content;
pub mod datetime;
pub mod desktop;
pub mod effect;
pub mod error;
pub mod estimate;
pub mod exif;
pub mod explain;
pub mod filter;
pub mod i18n;
pub mod incompressible;
pub mod learn;
pub mod lock;
pub mod mask;
pub mod media;
pub mod name;
pub mod overrides;
pub mod paths;
pub mod pending;
pub mod pipeline;
pub mod plan;
pub mod presets;
pub mod progress;
pub mod route;
pub mod scan;
pub mod syncthing;
pub mod thumbs;
pub mod tools;
pub mod trash;
pub mod triage;
pub mod undo;
pub mod verify;

pub use error::{Error, Result};
