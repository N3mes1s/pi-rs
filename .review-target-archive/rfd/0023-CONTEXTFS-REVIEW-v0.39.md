# RFD 0023 v0.39 — contextfs maintainer review

> Reviewed by Giuseppe (contextfs maintainer) via Claude on 2026-05-05
> against contextfs `main` at commit `06e9531` (post-RFD-0024 PR 3
> part 7 + RFD-0025 maintainer review). Pi-rs RFD 0023 sits at v0.39
> READY, but several §3.5 subsections cite contextfs surfaces that
> have moved since pi-rs's last sync. **Targeted v0.40 fixups
> needed** before Commit G implementation lands. Body of the RFD
> is sound; the architectural choices are good. Issues are
> concrete and small.

## Findings (in load-bearing order)

### 1. §3.5.4 audit-ping is fully obsolete — needs rewrite

**Problem.** Pi-rs's §3.5.4 cites `Request::WriteAuditPing`,
per-mount `audit_ping = { mode, high_water_mark }` TOML opt-in,
and `fail-open`/`fail-closed` semantics. **All of this has been
removed from contextfs** in PR 3 part 7 (commit `655408d`,
2026-05-04). Replaced wholesale by RFD-0024's audit tunnel
(`AuditPusher`).

**What changed (authoritative):**
- `Request::WriteAuditPing` and `Response::AuditPingAck` —
  deleted from the broker wire protocol.
- `crates/contextfsd/src/audit_ping.rs` — deleted entirely.
- `AuditPingConfig` / `AuditPingMode` / `audit_ping` TOML field —
  deleted from `crates/contextfsd/src/config.rs`. **TOML still
  containing the `audit_ping` field will be rejected at daemon
  startup** by `#[serde(deny_unknown_fields)]`.
- The 6 fail-closed gates in `fuse_bridge.rs` (write, create,
  rename, setattr, xattr.write, xattr.delete) are removed too —
  the failure model is now "daemon never blocks FUSE on broker
  reachability; chain is local, push is best-effort batched."
- Replacement: `AuditPusher` consumes records via the in-process
  broadcast, batches them (default size 256, coalesce 1s), pushes
  to broker as `Request::AuditPush` with v2 envelopes. No TOML
  knob; on by default when `[broker].socket_path` is set.

**Action for v0.40:**
- Delete §3.5.4 entirely or replace with a one-paragraph note
  that contextfs PR 3 ships always-on batched audit push, no
  per-mount opt-in, no fail-closed gate.
- §3.5.7 TOML must drop the `audit_ping = { ... }` line. Will
  fail loud (deny_unknown_fields) the moment Commit G boots.

### 2. AuditResync at startup requires shared tenant secret — §3.5.2 + §3.5.6 incomplete

**Problem.** Per RFD-0024 PR 3 part 5, the daemon dials
`AuditResync` against the broker BEFORE the first mount. The
broker can only answer if it has the same tenant secret bytes
the daemon is HMAC-signing with. Pi-rs's §3.5.2 derives a
per-VM secret host-side, writes it to `<run_dir>/<vm_id>/cfs-tenant-secret`,
and bind-mounts into the guest at
`/var/run/cfs/tenant_secret` — that's correct for the daemon's
side. But §3.5.6 (broker invocation) **does not pass
`--tenant-secret-path`** to the broker. Without that, the broker
returns `verify_write_unavailable` on AuditResync and the
daemon refuses to mount.

**Authoritative reference.** Memory entry
`reference_rfd0024_broker_tenant_secret.md` and contextfs
commit `d796fc0` (smoke fix that uncovered this gap).

**Action for v0.40:**
- §3.5.6 broker invocation gains `--tenant-secret-path
  <run_dir>/<vm_id>/cfs-tenant-secret` (the same file pi-rs
  derives in §3.5.2 — both sides point at it).
- The broker's tenant-mode and the daemon's `vm_id`/`master_epoch`
  flow are unchanged; just the missing flag.

### 3. Topology mismatch: §3.5.5 has per-VM broker, RFD-0025 §A has one-per-pi-process

**Problem.** Pi-rs's RFD-0023 §3.5.5 diagrams a broker per VM
(host-side `contextfs-broker --listen-uds <run_dir>/<vm_id>/broker.sock`,
4 keypairs/VM, 2 cfs-mesh-bridges/VM). Pi-rs's own RFD-0025 §A
explicitly says "pi-rs runs ONE broker per pi process, not per
VM; the bridge fans VMs in." These are architecturally
different — a per-VM broker is N processes for N VMs; a shared
broker is 1 process serving N VMs (with `--tenant-peer-uid`
binding the bridge process's uid).

**Why this matters now.** Commit G can't implement both. The
operational story (one shared broker) is much cheaper to run
(N×0 brokers vs N×1) and matches the contextfs broker's
multi-tenant capability (`--tenant-peer-uid` is exactly that).
The §3.5.5 per-VM diagram pre-dates RFD-0025's clarification.

**Action for v0.40:**
- Pick one. RFD-0025's "shared broker" is the cheaper-to-operate
  shape and is what `crates/contextfs-broker` already supports
  natively (`--tenant`, `--tenant-peer-uid`, multiple tenants
  per broker process).
- §3.5.5 diagram updates: one host-side broker on `/run/pi-rs/broker.sock`
  (not per-VM), N pairs of cfs-mesh-bridges (per-VM data + control
  plane), per-VM tenant ids and peer-uid bindings.
- The tenant secret per VM (§3.5.2) is unchanged — the broker
  derives the same per-VM secret from `(master, tenant_id, vm_id, master_epoch)`.

### 4. `BrokerTenantModeMismatch` is dropped — §3.5.9 still lists it

**Problem.** Pi-rs RFD-0025 §C.1 explicitly dropped
`AcquireError::BrokerTenantModeMismatch { detail }`. RFD-0023
§3.5.9's failure-surface table still lists it (row:
`contextfsd rejects with tenant_mode_legacy_no_vm_id`). Conflict
with pi-rs's own decision in RFD-0025.

**Authoritative reference.** Pi-rs RFD-0025 v0.40 (planned)
cited contextfs's `StartError` taxonomy: `BrokerVmIdRequired`
covers the legacy-no-vm_id case; `BrokerProtocolTooOld` and
`AuditInstanceClosedOnBroker` are the new RFD-0024 variants
that need pi-rs `AcquireError` mappings.

**Action for v0.40:**
- Drop the `BrokerTenantModeMismatch` row from §3.5.9.
- Add rows for `BrokerProtocolTooOld { broker_socket, error }`
  and `AuditInstanceClosedOnBroker { daemon_instance_id, closed_at_seq }`.
  Map them per pi-rs RFD-0025's plan (operator-alert variants).

### 5. §3.5.7 TOML — missing `daemon_instance_id_path` (RFD-0024 PR 3)

**Problem.** RFD-0024 PR 3 part 5 made the daemon's instance id
load-bearing: it persists across restarts (defaults to
`<audit_log_path>.parent/daemon_instance.id`) so AuditResync
can find the broker's high_watermark for the same instance.
Pi-rs's §3.5.7 TOML doesn't set `daemon_instance_id_path`.
Default-path will work but it's worth being explicit so
operators can rotate it.

**Action for v0.40:**
- Add `daemon_instance_id_path = "/var/lib/contextfs/daemon_instance.id"`
  to §3.5.7 TOML for explicitness.
- Note in §3.5 boot contract: rotating this file forces a
  fresh AuditResync (broker treats new instance id as a fresh
  pair). Useful for warm-pool VMs after master_epoch rotation.

### 6. §3.5.8 version pin — RFD-0025 already resolved

**Problem.** §3.5.8 says "ContextFS broker MUST be `>= v0.3.0`"
but contextfs `Cargo.toml` is at `0.0.1-dev` with no git tags.
Pi-rs RFD-0025 §C.2 resolved this: pin by git rev now, flip to
`version = "0.1.0"` when contextfs cuts the tag.

**Action for v0.40:**
- §3.5.8 cites RFD-0025 §C.2 directly; drops the "v0.3.0" claim.
- Replace the "v0.2.x brokers reject any embedder request via
  serde unknown-field rejection" with: "pre-RFD-0024 brokers
  surface as `StartError::BrokerProtocolTooOld` from the
  daemon's `Hello` probe. Daemon refuses to mount; no silent
  degradation."

### 7. New surfaces pi-rs gets for free — worth a one-line mention each

**(a) Operator HTTP `POST /tenants/<t>/daemons/<id>/instance-close`**
(commit `655408d`, RFD-0025 §C.1 mentions). Pi-rs operators can
force-close a daemon's instance from the broker pane — useful
for "agent compromise detected, kill its audit chain
immediately" without waiting for VM teardown. Worth a footnote
in §3.5.6 or §"Operational considerations".

**(b) Kernel-attested `exe_realpath` + `start_time` in
`broker.log`** (commit `358bcb3`). Every `verify_write` decision
in `broker.log` now carries `caller_exe` (kernel symlink target,
not `comm`-spoofable) and `caller_start_time` (boot-relative
process birth-tick). Pi-rs's `SandboxAction` telemetry can pick
these up for free. One-line mention in §"Observability" closes
it.

**(c) `instance_closed` sentinel on graceful shutdown** (RFD-0024
PR 3 part 6, commit `604f897`). When pi-cfs-init calls
`handle.shutdown().await`, the daemon mints a signed sentinel
that the broker pins as the close watermark. Hard kills (warmpool
eviction without a graceful close) skip the sentinel — broker's
high_watermark stays intact, and the operator force-close path
above fills the gap if needed.

**Action for v0.40:** add the three one-liners. No design impact;
it's narrative completeness.

### 8. Lineage proofs — out of scope, mention once

Pi-rs's own §6 threat-model already covers the FS boundary;
adding "no Cedar gate on process lineage" as an explicit
non-goal preserves the reader's ability to find the deferred
design later (memory entry: `project_lineage_proofs_deferred.md`).
The eBPF-unavailable-in-target-sandboxes constraint applies to
pi-rs too — when this is revived, the cooperative HMAC token
mode will need pi-sandbox-worker (or pi-cfs-init) as the trust
root.

**Action for v0.40:** one bullet under §"Out of scope / deferred":
"Process lineage proofs as Cedar context — deferred at contextfs
side; revisit when pi-rs policies require ancestor-based gating."

## Things that are right and don't need to change

- **§3.5.1 vm_id source + lifetime + warm-pool sharing.** Matches
  contextfs RFD-0023 §5 exactly. No change.
- **§3.5.2 per-VM tenant secret derivation flow.** Correct; just
  needs the broker-side `--tenant-secret-path` flag added per
  finding #2.
- **§3.5.3 OIDC token bind-mount + rotation policy.** Correct;
  no change.
- **§3.5.5 Noise-IK keypair plumbing (4 keypairs/VM).** Correct
  for the cfs-mesh transport layer, regardless of whether the
  broker is per-VM (finding #3 mismatch) or shared. The keys are
  per-channel and per-VM either way.
- **§3.5.9 pi-cfs-init readiness gates without mutating `/work`.**
  Excellent — the "boot must not mutate /work" invariant is
  exactly the right design call.
- **The `acquire()` flow in §A of RFD-0025** matches the daemon
  startup sequence in `crates/contextfsd/src/lib.rs:start()`. No
  drift.
- **Schema-drift detection at TOML deserialize time** with a
  pinned-compat integration test gated on `CONTEXTFS_REPO_PATH`
  (§3.5.7) is the right shape. No change.

## Verdict

**v0.39 is `READY` for the body of the RFD; §3.5 needs v0.40
fixups before Commit G implementation lands.** The drift is
real but small — 7 concrete edits, no architectural rework.
Pi-rs's overall design (managed-mode-on-Linux, two-channel
cfs-mesh, pi-cfs-init readiness, schema-drift detection) is
sound and matches contextfs's shipped surfaces.

Recommendation: cut v0.40 with the 7 edits, run rfd-critic once
more (probably READY first pass), then proceed to Commit G.

If pi-rs side wants me to draft the actual diff for any of the
§3.5 subsections, ping me — happy to write the new §3.5.4 (the
audit_ping → AuditPusher rewrite) directly since that's the
heaviest lift and it's contextfs-shaped content.

— Giuseppe (contextfs maintainer), via Claude
  Reviewed against contextfs `main` @ `06e9531`
