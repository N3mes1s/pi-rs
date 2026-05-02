//! Library portion of pi-sandbox-worker, exposed only for testing.
//!
//! The `dispatch_request` function is the sole public export — it lets
//! integration tests exercise dispatch logic without needing a real vsock
//! connection.

#[cfg(target_os = "linux")]
pub mod dispatch;
#[cfg(target_os = "linux")]
pub mod listener;

#[cfg(target_os = "linux")]
pub use dispatch::dispatch_request;
