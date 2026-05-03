# pi-sdk

> ⚠ **Pre-1.0.** Any 0.x → 0.x+1 release MAY break the public API. Pin a fixed version.

The public Rust SDK for embedding the [pi-rs](https://github.com/n3mes1s/playground) coding-agent harness in another application. One dependency, one entry point — `cargo add pi-sdk` and write your agent.

## Status

This is the Commit-A scaffold per [RFD 0027](../../rfd/0027-pi-rs-sdk.md). The full README (with conceptual model diagram, production checklist, threat model, and end-to-end examples) lands in Commit F.

What ships in 0.1 (Commit A):

- Façade crate scaffold
- Re-exports of the embedder-facing types from `pi-tool-types`, `pi-ai`, `pi-tools`, `pi-sandbox`, `pi-agent-core`
- Convenience builder (`BuildConfig`, `build_runtime_config`) moved from `pi_coding_agent::sdk`
- Feature-flag matrix declared (currently inert; underlying-crate plumbing in follow-up commits)

What lands in follow-up commits:

- `RuntimeConfig::builder()` with blanket `#[non_exhaustive]` — Commit B
- Top-level `pi_sdk::Error` / `pi_sdk::Result` — Commit C
- `pi_sdk::mocks::{MockProvider, MockSandboxProvider}` — Commit D
- `pi_sdk::cost::{CostRegistry, estimate_cost_usd}` — Commit E
- Full README + 5 examples — Commit F
- Hardening (catch_unwind, stream validation, ToolGate ctx, bash jail, AuthStorage 0o600 + atomic, WireSerializer, default-surface renames) — Commits H1-H7
- Compatibility canary + supply-chain CI + crates.io publish — Commits G/I/J

## Quick start (current minimal shape)

```rust,no_run
use pi_sdk::{
    build_runtime_config, AgentEventKind, AgentSessionRuntime, AuthStorage,
    BuildConfig, Settings, ToolRegistry,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cfg = build_runtime_config(BuildConfig {
        auth: AuthStorage::from_env(),
        tools: ToolRegistry::with_defaults(),
        settings: Settings::default(),
        ..BuildConfig::default()
    });

    let runtime = AgentSessionRuntime::new(cfg);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let session = runtime.create_session(Some(tx))?;

    tokio::spawn(async move {
        let _ = session.prompt("List files in this directory.".into()).await;
    });

    while let Some(evt) = rx.recv().await {
        match evt.kind {
            AgentEventKind::AssistantTextDelta { text } => print!("{text}"),
            AgentEventKind::TurnComplete => break,
            _ => {}
        }
    }
    Ok(())
}
```

## License

MIT.

## See also

- [RFD 0027](../../rfd/0027-pi-rs-sdk.md) — design contract, threat model, hardening contract.
- [RFD 0023](../../rfd/0023-microvm-sandbox-provider.md) — local microVM sandbox.
- [RFD 0026](../../rfd/0026-remote-sandbox-transports.md) — remote sandbox transports.
