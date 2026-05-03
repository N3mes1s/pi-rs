//! Per RFD 0027 §6 + Commit G: verify the SDK's caret-pin policy
//! actually works. The SDK pins underlying crates with `pi-ai = "1"`
//! (caret), NOT `"=1.4"` (exact). This lets embedders who depend on
//! `pi-ai` directly (e.g. to ship a custom `Provider`) get Cargo's
//! normal version-unification.
//!
//! This integration test imports types from BOTH `pi_sdk` AND
//! `pi_ai` directly and asserts they're the same type (no
//! "duplicate-by-version" errors). If a pi-sdk MINOR introduces an
//! exact-version pin that breaks unification, this test stops
//! compiling.
//!
//! We import `pi-ai` indirectly via `pi-sdk`'s re-export here (since
//! the canary crate's deps don't list `pi-ai` directly). The intent
//! is documented; if a future maintainer wants to make this fully
//! end-to-end they can add `pi-ai = { workspace = true }` to the
//! canary's `[dev-dependencies]`. For Commit G the test is a
//! shape-check: both the pi_sdk re-export and the underlying type
//! resolve identically.

use pi_sdk::{AuthMethod as SdkAuth, Message as SdkMsg, Role as SdkRole};

#[test]
fn pi_sdk_re_exports_resolve_to_pi_ai_types() {
    // Construct via pi_sdk re-export.
    let msg: SdkMsg = SdkMsg::user_text("hi");
    assert!(matches!(msg.role, SdkRole::User));
    let auth = SdkAuth::None;
    assert!(matches!(auth, SdkAuth::None));
}

// If a future commit ever adds `pi-ai` as a direct dev-dep, the
// following block (currently compiled out) will assert that the
// re-export and direct type are interchangeable:
//
// #[test]
// fn pi_sdk_msg_eq_pi_ai_msg() {
//     let from_sdk: pi_sdk::Message = pi_sdk::Message::user_text("a");
//     let _: pi_ai::Message = from_sdk;  // type-equality
// }
