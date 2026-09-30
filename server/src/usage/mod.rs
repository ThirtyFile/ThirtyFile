//! Control panel › Storage usage: how much each storage location holds over time, and how its storage operations go.
//!
//! - meter.rs counts and times operations in memory, off the database;
//! - sample.rs writes them every five minutes, measures capacity every fifteen, and keeps the history bounded;
//! - api.rs answers the page from the samples and the counters, never from the disks themselves.

pub mod api;
pub mod meter;
pub mod sample;

pub use meter::{Meters, Metered, Op, background, probe};
