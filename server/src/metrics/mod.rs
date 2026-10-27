//! Monitoring: `/metrics` in the Prometheus text format, for a monitoring system the organisation already runs, and
//! Control panel › Monitoring, where an administrator turns it on and makes its token.
//!
//! - It is off until an administrator turns it on; it answers only to `Authorization: Bearer <token>` (a token kept as
//!   a hash, shown once), and optionally only to listed addresses.
//! - Nothing in it names a person or a file: labels are fixed words (a method, a kind of task), the names of storage
//!   locations and of backup policies, which administrators chose.
//! - A scrape reads memory only: counters kept as things happen, and the figures `collect.rs` looks at every fifteen
//!   seconds. It takes no write lock and reads no disk.

mod collect;
mod dashboard;
mod expose;
mod instruments;
mod render;
mod settings;

pub use collect::spawn as spawn_collector;
pub use dashboard::{get as get_dashboard, spawn as spawn_dashboard};
pub use instruments::{Memory, beat, email_failed, email_sent, sign_in_event, task_finished, track};
pub use render::scrape;
pub use settings::{Settings, get_settings, load, update_settings};

#[cfg(test)]
mod tests;
