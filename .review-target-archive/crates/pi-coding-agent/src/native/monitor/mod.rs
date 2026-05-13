//! RFD 0017 — `monitor` tool plumbing.
//!
//! * [`MonitorPump`] is a [`StreamInterceptor`] that drains queued
//!   [`MonitorNotification`]s before each turn and folds them into the
//!   next assistant request as a synthetic user message.
//! * [`spawn_event_bridge`] forwards `MonitorNotification`s onto the
//!   runtime's [`EventSender`] as `MonitorEvent` / `MonitorEnded`.

pub mod runtime_hook;

pub use runtime_hook::{spawn_event_bridge, MonitorPump};
