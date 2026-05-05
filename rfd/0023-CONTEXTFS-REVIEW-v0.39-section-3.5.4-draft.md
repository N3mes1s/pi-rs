# §3.5.4 — Audit chain forwarding (AuditPusher) — v0.40 draft

> Draft from the contextfs maintainer to splice into pi-rs RFD 0023
> §3.5.4. Replaces the existing audit-ping section wholesale.
> Reviewed against contextfs `main` @ `06e9531`. Pi-rs side splices
> as-is or edits for prose fit; the technical claims are
> authoritative.

---

#### 3.5.4 — Audit chain forwarding (AuditPusher)

ContextFS's RFD-0024 (commits `6594c3f → 06e9531`, "Embedder audit
tunnel") replaced the per-write `WriteAuditPing` pipeline with a
batched **AuditPusher** that runs as a background task in
`contextfsd`. The shape is fundamentally different from the audit-
ping it superseded: chain integrity is **local and reliable**, broker
push is **best-effort batched**, and write-class FUSE ops are never
gated on broker reachability. Pi-rs operators get the new surface for
free — no per-mount config, no fail-closed knob, no daemon-side TOML
opt-in.

##### What the daemon does (in-guest)

Every successful FUSE op (read / write / list / stat / xattr / …)
produces an HMAC-chained record in `audit_log_path`'s ndjson. A
single subscriber (the AuditPusher; subscriber count is enforced = 1
by the daemon's broadcast primitive) consumes records, batches them,
encrypts each batch as a `WriteLogPayload::AuditBatch` v2 envelope
under the per-VM tenant secret derived in §3.5.2, and pushes via
`Request::AuditPush` to the broker. Defaults: batch size 256
records, coalesce window 1 s. The push is async — FUSE ops never
await it. On transport failure (broker UDS gone, timeout) the
pusher drops the in-flight batch, logs `audit_pusher_transport_dropped`
with the seq range, and continues consuming. The local audit chain
remains intact regardless; the broker sees a seq gap on the next
successful push and the operator pane surfaces it.

##### What the broker does (host-side)

The broker maintains an in-memory `AuditReplayState` keyed on
`(tenant_id, daemon_instance_id)` with a 5-row first-match-wins
acceptance table: `InstanceClosed → SeqRegression → IdempotentRetry
→ NonceReuseEnvelopeMismatch → Accept`. Per-batch HMAC verification
uses the same per-VM tenant secret bytes the daemon signs with —
which means the broker MUST be invoked with `--tenant-secret-path`
pointing at the same file pi-rs derives in §3.5.2 (see §3.5.6
finding). On accept, `high_watermark` advances to the batch's
`audit_seq_max`. Each accepted batch lands as a `tracing::info!`
record in `broker.log` carrying the daemon's
`(tenant_id, daemon_instance_id, audit_seq_min, audit_seq_max,
push_nonce)` plus the `verify_write` decision fields per record
(`caller_pid`, `caller_uid`, `caller_comm`, `caller_cmdline`,
`caller_ppid`, `caller_ppid_comm`, and — as of contextfs commit
`358bcb3` — `caller_exe` (kernel-attested realpath of
`/proc/<pid>/exe`) and `caller_start_time` (boot-relative
birth-tick)).

##### Startup handshake — `AuditResync`

At daemon boot, BEFORE any mount, the daemon dials
`Request::AuditResync` against the broker. The broker returns its
recorded `(high_watermark, closed_at_seq)` for the daemon's
`(tenant, vm_id, daemon_instance_id)` triple. The daemon:

- Drops local records with `seq <= high_watermark` (already accepted
  broker-side; re-pushing would be a `SeqRegression`).
- Resumes the chain at `high_watermark + 1`.
- Refuses to mount with typed `StartError::AuditInstanceClosedOnBroker`
  if the broker reports `closed_at_seq.is_some()`. Recovery: rotate
  `daemon_instance_id_path` and restart.

A pre-RFD-0024 broker (no `Hello`/`AuditResync` support) is caught
at the earlier `Hello` probe with typed
`StartError::BrokerProtocolTooOld` — the daemon refuses to mount
rather than degrade silently.

##### Lag handling

If the daemon emits records faster than the pusher can drain (e.g.
`cargo build` burst on a flaky broker connection), the bounded
broadcast ring overflows. The pusher consumes a `RecvError::Lagged`
event carrying `(dropped_count, oldest_dropped_seq, newest_dropped_seq)`,
emits a chain-stamped `audit_subscribe_lagged` record into the
audit log via a no-broadcast writer (Weak-backed handle, can't
recurse), rate-limited to 1/sec. The chain witness is forge-evident
(HMAC-keyed) and visible to the operator pane via the next AuditPush.

##### Graceful shutdown — `instance_closed` sentinel

When pi-cfs-init calls `handle.shutdown().await`, the daemon mints
a signed `instance_closed` sentinel (HMAC-BLAKE3-keyed under the
per-VM secret, length-prefixed canonical signing message including
`tenant_id`, `vm_id`, `daemon_instance_id`, `audit_seq_high`) and
sends it to the broker as `Request::InstanceClosed`. The broker
verifies the HMAC AND that `audit_seq_high` byte-equals its own
recorded `high_watermark` (round-3 red-team forge guard); if both
hold, the broker pins `closed_at_seq = audit_seq_high` and refuses
every subsequent `AuditPush` for that pair. Hard-killed VMs (warmpool
eviction without a graceful close) skip the sentinel — the broker's
high_watermark stays intact, and the operator force-close path below
fills the gap if the operator wants the chain hard-closed.

##### Operator force-close — kill-switch

ContextFS's broker exposes
`POST /tenants/<tenant>/daemons/<daemon_instance_id>/instance-close`
on its localhost+bearer operator HTTP pane (commit `655408d`).
Operators with detected agent compromise call this endpoint to pin
`closed_at_seq` at the broker's current `high_watermark` immediately.
Idempotent (second call returns `forced: false` with the prior
`closed_at_seq`). A daemon attempting `AuditPush` against a
force-closed pair sees `replay_detected{cause: InstanceClosed}` and
exits the push loop; pi-rs operators see this as the equivalent of
"agent's audit chain hard-closed, no more writes accepted." Useful
for hostile-tenancy / compliance-graded incident response.

##### Pi-rs config implications

- **No new MicroVmProviderConfig fields** — the AuditPusher is
  always-on whenever `[broker].socket_path` is set in the daemon
  TOML (which §3.5.7 always sets in managed mode).
- **No `audit_ping = { … }` TOML field** — it was removed from
  contextfs's config schema in commit `655408d`. Daemons starting
  with that field in their TOML will fail loud at config load
  time (`#[serde(deny_unknown_fields)]`).
- **`daemon_instance_id_path`** controls the file the daemon
  persists its instance UUID to. Default is
  `<audit_log_path>.parent/daemon_instance.id`. Rotate it (delete
  the file before the next boot) to force a fresh
  `(tenant, daemon_instance_id)` pair at the broker — useful after
  master_epoch rotation.
- **Throughput knob (optional):** the per-mount
  `[verify_write_batch]` TOML table opts into batched
  `verify_write` calls (RFD-0024 PR 2). Pi-rs deployments with
  bursty `cargo build`-style writes can reduce broker round-trip
  pressure by enabling it; default-off and the daemon refuses to
  start with an out-of-range value (no silent clamp).

##### Failure surface added since v0.39 — for §3.5.9 table

| Trigger | `AcquireError` variant |
| --- | --- |
| Broker pre-RFD-0024 (no `Hello` ack) | `BrokerProtocolTooOld { broker_socket, error }` |
| Broker reports `closed_at_seq.is_some()` for this `(tenant, daemon_instance_id)` | `AuditInstanceClosedOnBroker { daemon_instance_id, closed_at_seq }` |

Both are `StartError` variants on the contextfs side; pi-rs maps
them per RFD-0025 §C.1 as operator-alert variants.

##### What pi-rs does NOT get from the audit-ping → AuditPusher
swap

- **No fail-closed gate on FUSE writes when broker is unreachable.**
  Audit-ping's `mode = "fail-closed"` blocked write-class FUSE ops
  with EIO when the per-mount channel was saturated. AuditPusher
  has no such gate — by design. The local audit chain is the source
  of truth for forensic claims; broker push is best-effort
  replication. Operators who want broker-roundtrip-or-bust on
  every write should use the operator force-close path or build
  their own watchdog on `broker.log`'s `audit_pusher_transport_dropped`
  events. RFD-0024's threat model treats agent-blocking on broker
  liveness as a worse failure mode than bounded audit-replication
  lag.

##### References

- contextfs RFD-0024 §"Refactor scope" items 1–6 (audit broadcast,
  pusher, replay state, sentinels, AuditResync, Hello probe).
- `crates/contextfs-core/src/audit_broadcast.rs` — single-subscriber
  bounded broadcast.
- `crates/contextfsd/src/audit_pusher.rs` — daemon-side batcher.
- `crates/contextfs-broker/src/audit_replay.rs` — 5-row replay table.
- `crates/contextfs-core/src/instance_close.rs` — sentinel HMAC.
- `crates/contextfs-broker/src/operator_http.rs` —
  `POST /tenants/<t>/daemons/<id>/instance-close`.
- `reference_rfd0024_broker_tenant_secret.md` — bring-up gotcha
  (broker MUST share the daemon's tenant secret).
