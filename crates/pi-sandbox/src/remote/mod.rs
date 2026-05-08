//! Remote sandbox providers — cloud-hosted microVM-as-a-service backends.
//!
//! v1 ships the E2B reference implementation. Sprites and Daytona are
//! deferred to post-v1 (RFD 0026 §"Commit H and I").
//!
//! No `#[cfg]` gate: the remote module is always compiled. Runtime
//! gating is done via API-key presence at session open.

pub mod e2b;
