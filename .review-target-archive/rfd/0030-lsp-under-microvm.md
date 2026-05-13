# RFD 0030 — `lsp` under sandboxed providers (microvm + remote)

| Status | Discussion |
| ------ | ---------- |
| Author | opus-4.7 (1M context) |
| Date   | 2026-05-06 |
| Depends on | RFD 0023 (microvm sandbox provider), RFD 0026 (remote backends), RFD 0001 (LSP write hook) |
| Supersedes | none |
| Related | RFD 0017 (monitor — cousin tool with a similar streaming-protocol mismatch, see RFD 0031) |

## Status note

RFD 0023 v1 marks `lsp` as `ToolDispatch::Unavailable` under
microvm/remote providers. Operators who want LSP must run with
`--sandbox-provider=local-process`. This RFD proposes the path to
**make `lsp` actually work** under sandboxed providers without
abandoning the sandbox boundary.

## Summary

`lsp` (introduced in RFD 0001 + the engine work in
`crates/pi-coding-agent/src/native/lsp/`) drives long-lived language
servers (rust-analyzer, pyright, tsserver, …) over JSON-RPC and
exposes 11 ops to the agent (definition, references, completion,
hover, …). Three architectural mismatches make it incompatible
with microvm-style sandboxes today:

1. Language servers are **heavy host processes** (multi-second
   startup, GBs of indexing memory, not in alpine rootfs).
2. LSP identifies files by **absolute host-path URIs** that
   guest paths don't match.
3. LSP is **heavily stateful across calls** — `initialize` →
   many `didOpen` / `didChange` → many queries → `shutdown`. The
   one-shot ToolRequest/ToolResponse RPC can't carry that.

This RFD picks the **vsock-proxy + path translation + workspace-
pinned-VM** path (Option B below). The host keeps its existing
`LspEngine` + per-language `LspClient` instances; the guest worker
exposes a stub `lsp` tool that proxies requests to the host over a
new per-VM vsock channel; path URIs are translated at the proxy
boundary; warm-pool partitioning ensures the same VM serves every
call in one logical "LSP session" so the host's per-server state
remains valid.

## Background

### Why `lsp` is currently `Unavailable`

`crates/pi-coding-agent/src/native/lsp/engine.rs:LspEngine::spawn()`
shells out to `tokio::process::Command::new("rust-analyzer")` (and
similar binaries) on the **host**. Inside the guest:

- These binaries are not present in the alpine miniroot (RFD 0023
  Phase 2 rootfs).
- Even if installed via `apk add rust-analyzer` (or pre-baked),
  cold boot + first-call indexing of a real project costs
  multi-second wall time and ~1 GB RAM **per VM**. The warm pool
  retires VMs after 50 calls or 5 minutes; every retirement blows
  the index. Operationally untenable.

LSP's URIs are absolute host paths:

```jsonc
// Host POV
{ "uri": "file:///home/nemesis/code/myproject/src/main.rs",
  "position": { "line": 5, "character": 10 } }
```

In v1 the guest can't see that path at all (Firecracker silently
drops the virtio-fs `fs` device — RFD 0023 §"Filesystem
semantics"). After RFD 0023's Commit G3 (contextfs `/work`
integration) the guest will see the project at `/work/src/main.rs`
— but the host's rust-analyzer indexed the original host path,
not the guest path. Without translation, every `definition` reply
references files the agent inside the guest can't open.

LSP is fundamentally stateful. One agent "rename `foo` to `bar`"
maps to:

```
initialize     → server starts indexing
workspace/didChangeConfiguration
textDocument/didOpen { uri: …main.rs, text: <full content> }
textDocument/didOpen { uri: …lib.rs,  text: <full content> }
textDocument/rename  { uri: …main.rs, position: …, newName: "bar" }
… → WorkspaceEdit reply with edits across N files …
textDocument/didChange  { uri: …main.rs, version: 2, …deltas }
textDocument/didSave    { uri: …main.rs }
shutdown
```

The current sandbox protocol delivers one `ToolRequest` →
`ToolResponse` pair per call. `ToolRequest::tool_name == "lsp"` +
`tool_input == { op: "rename", … }` describes one of those bullets,
not the whole session. Without state continuity (same LS process
seeing all of those calls in order, with `didChange` versions
matching), the server returns wrong or empty results.

### Why this matters

LSP is the highest-value-per-call tool the agent has for code work.
Definition jumps, hover, completion, find-references, rename — when
they work, they collapse 20-tool-call grep loops into one. Forcing
operators to choose between "sandbox boundary" and "LSP support"
is a real UX regression vs `local-process` mode.

## Design space

### Option A — Bake language servers into the rootfs, run guest-side

Pre-`apk add` rust-analyzer / pyright / tsserver / etc. into the
rootfs at build time. Worker spawns the LS in-guest via the
existing `LspEngine`. Path URIs are guest paths (`/work/...`).

| Pro                                              | Con                                                                           |
|--------------------------------------------------|-------------------------------------------------------------------------------|
| No new wire protocol                             | Rootfs grows from ~80 MiB to multi-GiB (rust-analyzer alone is ~150 MiB)      |
| State stays in one place                         | Cold boot includes LS startup (~5s) and full re-indexing                      |
| No path translation (everything is `/work/*`)    | Warm pool retirement (`MAX_CALLS=50`) blows the index every rotation          |
| Trivial trust boundary (LS sees only guest data) | Doesn't compose with `--sandbox-provider=remote:*` — remote backends won't ship rust-analyzer per-VM |

**Verdict: rejected for v1.** Operationally untenable. Re-evaluate
if remote backends ever offer "long-lived dev container with LS
pre-warmed."

### Option B — Vsock proxy to host's existing LspEngine (chosen)

Host keeps its `LspEngine` exactly as today. Guest worker exposes a
stub `lsp` tool that opens a per-VM vsock connection to the host
on a dedicated port (`VSOCK_LSP_PORT = 5004`, sibling of
`VSOCK_SEARCH_PORT = 5003`), serializes the same JSON-RPC frames the
LS speaks natively, and proxies them. Path URIs are **translated at
the proxy boundary**: guest `/work/src/main.rs` becomes host
`<canonical_host_cwd>/src/main.rs` on the way out, and back the
other way on response.

| Pro                                                                              | Con                                                                            |
|----------------------------------------------------------------------------------|--------------------------------------------------------------------------------|
| Reuses 100% of existing `LspEngine` + `LspClient` work (RFD 0001)                | New wire protocol surface (`pi-lsp-proto` crate, ~100 LoC)                      |
| Index lives once on host, shared across VM rotations                             | Path translation logic + tests                                                 |
| Compatible with remote backends (host stays in the embedder's process either way) | Warm-pool partitioning: a VM is bound to one workspace for life                |
| Aligns with the `web_search` proxy pattern (RFD 0023 §"web_search via vsock proxy") | Trust boundary widens: host is reachable from guest through one more vsock port (mitigated by seccomp + nft) |

**Chosen for v1.** Mirrors the wired-and-working `web_search`
proxy. Composable with everything we already have.

### Option C — contextfs-mediated, in-guest LS sees host files

Wait for RFD 0023 Commit G3 (contextfs `/work`) to land, then run
the LS in-guest reading files via contextfs FUSE. URIs are
`/work/...` paths backed by host file content.

| Pro                                          | Con                                                                                 |
|----------------------------------------------|-------------------------------------------------------------------------------------|
| URIs unify (no translation needed)            | Hard-blocks on G3                                                                   |
| LS sees real file content live               | Per-VM LS process, per-VM index — same problem as Option A                          |
| Tightest sandbox boundary                    | contextfs FUSE adds ~1 ms per file open; LSP indexing reads thousands of files     |

**Verdict: defer.** Reconsider after G3 lands AND if the per-VM
indexing cost can be amortized (e.g. shared rust-analyzer cache
mounted via contextfs read-only). Probably v2.0+.

## Proposal — Option B in detail

### 1. New crate `pi-lsp-proto`

```rust
// crates/pi-lsp-proto/src/lib.rs (~100 LoC, mirrors pi-search-proto)

pub const VSOCK_LSP_PORT: u32 = 5004;
pub const HOST_CID: u32 = 2;
pub const CURRENT_PROTO_VERSION: u32 = 1;

/// A single agent-issued `lsp` tool call. Mirrors the existing
/// `lsp` tool input shape (`crates/pi-coding-agent/src/native/lsp/
/// tool.rs:ToolSpec.input_schema`), one frame.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LspRequest {
    pub proto_version: u32,
    pub call_id: String,
    pub op: String,            // "definition" | "references" | …
    pub path: String,          // GUEST path (e.g. /work/src/main.rs)
    pub line: Option<u32>,
    pub col: Option<u32>,
    /// Optional extras (rename's new_name, completion's trigger char, …).
    pub extras: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LspResponse {
    pub proto_version: u32,
    pub call_id: String,
    /// Server reply, with paths already translated GUEST→HOST→GUEST
    /// at the proxy boundary so the agent sees `/work/...` URIs.
    pub result: serde_json::Value,
    pub error: Option<String>,
}

// Newline-JSON framing identical to pi-search-proto.
```

### 2. Host-side listener (per-VM)

`crates/pi-sandbox/src/microvm/lsp_proxy.rs` — sibling of
`search_proxy.rs`. Bound at `<vsock_path>_5004` when the launcher
spawns a VM with `lsp` enabled (gated, see §"Configuration"
below). On accept:

1. Read `LspRequest` frame.
2. Translate `request.path` GUEST → HOST: replace `/work` prefix
   with the VM's `host_cwd` (the canonical host path the VM was
   booted with).
3. Translate `request.extras` paths similarly (e.g. `rename` op
   has no path-bearing fields beyond `path`; `completion` may
   carry trigger context with file URIs — apply path translator
   recursively).
4. Forward to host's `LspEngine` via the existing `LspTool::invoke`
   surface (same `ToolContext::cwd` = `host_cwd`).
5. Translate the **reply's** path URIs HOST → GUEST: every
   `Uri.toString()` containing `host_cwd` becomes `/work/...`.
6. Wrap in `LspResponse` and ship back.

Path translation is the only non-trivial new code. Reuse
`crates/pi-coding-agent/src/native/lsp/ops.rs::traverse_uris` (or
add it) — a small helper that recursively walks a `serde_json::
Value` and rewrites every `"uri": "file://..."` it finds.

### 3. Guest worker stub

`crates/pi-sandbox-worker/src/dispatch.rs` gains an `lsp` arm
parallel to the existing `web_search` proxy arm:

```rust
if req.tool_name == "lsp" {
    return proxy_lsp(req).await;
}
```

`proxy_lsp` opens vsock(2, 5004), writes `LspRequest`, reads
`LspResponse`, returns `ToolResponse`. ~50 LoC.

### 4. Workspace pinning — pool-key partition

LSP state lives in the host's per-language `LspClient`, which is
keyed on `(language, workspace_root)` inside `LspEngine`. The
existing engine maintains one client per language for the lifetime
of the runtime — VMs come and go, the LS doesn't.

But the proxy listener is per-VM. Two VMs cold-booted with
different `host_cwd` route to **different** `LspEngine` workspace
roots — the engine keys the client correctly. So no engine-side
work is needed.

What IS needed: warm-pool **partitioning by `host_cwd`** (already
exists, RFD 0023 §"BootSpec"). Pool reuse is correct: same
workspace = same VM = same host LS client = state continuity
across many tool calls in one agent session.

### 5. Configuration

LSP under microvm is **opt-in** at the launcher level:

```rust
pub struct FirecrackerConfig {
    // …
    /// When true, cold_boot binds <vsock_path>_5004 and starts the
    /// per-VM LSP proxy listener. Default false (matches v1
    /// `Unavailable` behavior; operators flip this when they want
    /// LSP support under microvm). The host's `LspEngine` MUST be
    /// reachable from this process — it isn't part of the launcher
    /// crate, so the embedder injects an `Arc<dyn LspProxyHandler>`
    /// callback.
    pub lsp_proxy_handler: Option<Arc<dyn LspProxyHandler>>,
}
```

`LspProxyHandler` is a tiny trait the embedder (`pi-coding-agent`)
implements by wrapping its existing `LspEngine`. Keeps `pi-sandbox`
free of `pi-coding-agent` deps — same pattern as `web_search`'s
embedder-injection pattern (planned in RFD 0023, currently just
calls `pi_tools::WebSearchTool::default()` because the tool itself
lives below pi-coding-agent in the dep graph).

Tool-disposition wires it: when the embedder has set
`lsp_proxy_handler`, the runtime overrides `LspTool::dispatch()`
to return `Guest` instead of `Unavailable`. When not set, the
`Unavailable` path stays in force and the agent gets the same clean
error as today.

### 6. Threat model deltas

The vsock-5004 channel is reachable from the guest, so a
prompt-injected guest payload could in theory call into the host's
`LspEngine` directly:

- **Bash bypass**: blocked by RFD 0023's seccomp filter — bash
  inside the guest can't `socket(AF_VSOCK, ...)`. Same defense
  that protects vsock-5003 (search proxy).
- **Worker-issued malicious LSP requests**: the guest worker is
  trusted (it's pi-rs code we built). The threat is "what can a
  malformed `LspRequest` make the host's LS do?" Limited:
  - LS reads files within `host_cwd` (workspace root) by design
  - LSP doesn't have a "execute arbitrary code" op
  - `rename` returns edits; it doesn't apply them — the agent
    runs separate `write` tool calls to apply
- **Path traversal via crafted URIs**: the path translator at the
  proxy boundary canonicalises before substitution and rejects
  anything that doesn't resolve under `host_cwd`. Test:
  `request.path = "/work/../../etc/passwd"` is rejected; the
  reply doesn't surface paths outside `host_cwd`.

Net new attack surface: small. Net new defenses: path-canon-
icalization in the proxy.

### 7. Wire-format examples

`definition` op end-to-end:

```jsonc
// Agent → guest worker (existing ToolRequest)
{ "tool_name": "lsp",
  "tool_input": { "op": "definition", "path": "/work/src/main.rs",
                  "line": 12, "col": 7 } }

// Worker → host over vsock 5004 (LspRequest, newline-JSON)
{"proto_version":1,"call_id":"abc","op":"definition",
 "path":"/work/src/main.rs","line":12,"col":7,"extras":{}}

// Host's path translator: "/work/src/main.rs" → "/home/me/proj/src/main.rs"
// Host's LspEngine.invoke({op:"definition", path:"/home/me/proj/src/main.rs", ...})
// rust-analyzer reply (via existing client.rs):
// { "uri": "file:///home/me/proj/src/lib.rs", "range": { ... } }
// Path translator: "file:///home/me/proj/src/lib.rs" → "file:///work/src/lib.rs"

// Host → worker (LspResponse)
{"proto_version":1,"call_id":"abc",
 "result":{"uri":"file:///work/src/lib.rs","range":{...}},
 "error":null}

// Worker → agent (ToolResponse)
{ "stdout": "{\"uri\":\"file:///work/src/lib.rs\",\"range\":{...}}",
  "is_error": false, ... }
```

### 8. Failures, deadlines, deadlocks

- **LS startup**: first call per-language pays ~5 s indexing. Use
  `CallLimits::wall_timeout` (default 60 s) — index time fits.
  Document that the first `lsp` call after a cold boot costs the
  index time even though the LS itself runs on the host (fresh
  worker = fresh `LspEngine` instance per `pi` process).
- **Vsock disconnect mid-call**: if the host listener dies (e.g.
  embedder shut down `LspEngine`), the worker reads EOF, returns
  `is_error=true` with "lsp vsock proxy error: …" (parallel to
  `web_search`'s error path).
- **Path translator gets a path it can't resolve**: respond with
  `error: "path outside workspace: <path>"`. Don't fall through to
  the LS with un-translated paths.
- **LS replies with broken UTF-8 / oversized payload**: the framing
  cap (`DEFAULT_MAX_LINE_BYTES = 1 MiB`) rejects oversize.
  Workspace-symbols replies on large repos can exceed this; bump
  cap to 8 MiB for `lsp_proxy.rs` (LSP needs more than search).

## Implementation plan

| Commit  | Description                                                                                       | Est LoC |
|---------|---------------------------------------------------------------------------------------------------|---------|
| `30-A`  | New crate `pi-lsp-proto` (wire types + framing + 5 unit tests).                                   | 250     |
| `30-B`  | `LspProxyHandler` trait + `LspTool::dispatch()` upgrade so disposition keys on handler presence.  | 100     |
| `30-C`  | Host-side `lsp_proxy.rs` listener + path translator + 8 unit tests for the translator.            | 250     |
| `30-D`  | Guest worker `proxy_lsp` arm + integration with `lsp` tool name.                                  | 80      |
| `30-E`  | Embedder wiring (`pi-coding-agent` injects `LspEngine`-backed handler when LSP is enabled).       | 80      |
| `30-F`  | Integration test: agent calls `lsp` via microvm, asserts a real `definition` reply with `/work/`-translated URIs (host runs rust-analyzer against a fixture project). Gated on `PI_SANDBOX_FC_LSP_TEST=1`. | 200 |
| `30-G`  | Documentation: NETWORKING.md adds an LSP-proxy section; RFD 0023 unmarks `lsp` from `Unavailable` in the tool-disposition matrix.                                            | 100     |

**Total: ~1060 LoC across 7 commits.**

## Out of scope / deferred

- **Streaming notifications** (`window/logMessage`,
  `textDocument/publishDiagnostics`). LSP normally pushes
  diagnostics asynchronously; the proxy's request/response shape
  drops them. v1 acceptable: agent calls `diagnostics` op
  explicitly and gets a snapshot. Streaming push is RFD 0031's
  problem (monitor-style notifications).
- **Multi-workspace sessions**. Today one VM = one workspace. An
  agent that needs cross-repo LSP queries spawns one task-tool
  child per workspace. Multi-workspace support is a v2 feature.
- **In-guest LS** (Option A) for compute-heavy languages where
  the user wants per-VM isolation. Re-open if remote backends
  start offering pre-warmed dev containers.

## Open questions

1. **Where does the `LspEngine` lifecycle live when remote
   backends are in play?** RFD 0026's `RemoteVmLauncher` runs the
   VM elsewhere; the host's `LspEngine` runs locally on the
   embedder's machine. Vsock-5004 doesn't reach across that gap.
   Likely answer: remote backends use a different transport
   (HTTP-over-the-broker), and the proxy abstraction generalises.
   But this RFD doesn't solve it — it scopes to local microvm.

2. **Path translator: Windows path normalization?** When pi runs on
   Windows (Commit F = cloud-hypervisor), `host_cwd` may be
   `C:\Users\me\proj` and the LSP URI is
   `file:///C%3A/Users/me/proj/src/main.rs`. Translator needs
   per-OS canonicalisation. Probably belongs in a small shared
   helper rather than per-platform forks.

3. **Should the proxy speak raw JSON-RPC instead of a wrapped
   request/response?** LSP IS JSON-RPC; we could let the agent
   send raw LSP messages and the host pass them through with path
   translation only. Pro: smaller proto crate. Con: leaks the
   full LSP surface into the agent's prompt vocabulary, including
   notifications/cancel-request/progress that we currently
   abstract away.

## Revision history

- **v0.1 (2026-05-06):** Initial draft. Picks Option B (vsock
  proxy + path translation + workspace-pinned VM). Defers
  streaming notifications to RFD 0031.
