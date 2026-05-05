# §3.5.4 — Audit batching via `AuditPusher` (RFD-0024 PR 3) — v0.40 splice-ready

> v2 draft, trimmed against pi-rs v0.40-prep (commit `d6cdafa`) so
> §3.5.4 doesn't duplicate §3.5.6's `--tenant-secret-path` text,
> §3.5.9's failure-surface rows, or §3.5.10's free-surface mentions.
> Reviewed against contextfs `main` @ `06e9531`. Pi-rs side splices
> the body below in place of the PENDING placeholder; the heading
> `#### 3.5.4 — Audit batching via AuditPusher (RFD-0024 PR 3)` is
> unchanged from v0.40-prep.

---

#### 3.5.4 — Audit batching via `AuditPusher` (RFD-0024 PR 3)

ContextFS PR 3 (commits `6594c3f → 06e9531`, "Embedder audit
tunnel") replaced the per-write `WriteAuditPing` pipeline with a
batched **AuditPusher** that runs as a background task in
`contextfsd`. The shape is fundamentally different from the audit-
ping it superseded: chain integrity is **local and reliable**, broker
push is **best-effort batched**, and write-class FUSE ops are never
gated on broker reachability. Pi-rs's `MicroVmProviderConfig` gains
no new fields — the AuditPusher is always-on whenever
`[broker].socket_path` is set, which §3.5.7 always sets in managed
mode.

##### Daemon-side flow (in-guest)

Every successful FUSE op (read, write, list, stat, xattr, …) lands
an HMAC-chained record in `audit_log_path`'s ndjson. A single
subscriber (the AuditPusher; subscriber count is enforced = 1 by
contextfs's broadcast primitive) consumes records, batches them,
and encrypts each batch as a `WriteLogPayload::AuditBatch` v2
envelope under the per-VM tenant secret derived in §3.5.2. The
batch ships as `Request::AuditPush` to the shared broker over the
control-plane channel (§3.5.5). Defaults: 256 records per push,
1 s coalesce window. The push is async — FUSE ops never await it.
On transport failure (broker UDS gone, timeout) the pusher drops
the in-flight batch, logs `audit_pusher_transport_dropped` with
the seq range, and continues consuming. The local audit chain
remains intact regardless; the broker sees a seq gap on the next
successful push, which the operator pane surfaces.

##### Broker-side acceptance (host-side)

The broker maintains an in-memory `AuditReplayState` keyed on
`(tenant_id, daemon_instance_id)` with a 5-row first-match-wins
acceptance table — `InstanceClosed → SeqRegression →
IdempotentRetry → NonceReuseEnvelopeMismatch → Accept`. Per-batch
HMAC verification uses the per-VM tenant secret bytes pi-rs
shipped to the broker via `--tenant-secret-path` (§3.5.6). On
accept, `high_watermark` advances to the batch's `audit_seq_max`
and the broker emits a `tracing::info!` record per accepted
record into `broker.log` carrying the daemon's
`(tenant_id, daemon_instance_id, audit_seq_min, audit_seq_max,
push_nonce)` plus the per-record `verify_write` decision fields
(see §3.5.10 for `caller_exe` / `caller_start_time`).

##### Startup handshake — `AuditResync`

At daemon boot, BEFORE any mount, the daemon dials
`Request::AuditResync` against the broker. The broker returns its
recorded `(high_watermark, closed_at_seq)` for the daemon's
`(tenant, vm_id, daemon_instance_id)` triple. The daemon then:

- Drops any local records with `seq <= high_watermark` (already
  accepted broker-side; re-pushing would produce a `SeqRegression`
  rejection).
- Resumes the chain at `high_watermark + 1`.
- Refuses to mount with typed
  `StartError::AuditInstanceClosedOnBroker` if the broker reports
  `closed_at_seq.is_some()`. Recovery: rotate
  `daemon_instance_id_path` (§3.5.7) and restart, which produces a
  fresh pair the broker treats as a new instance.

A pre-RFD-0024 broker (no `Hello`/`AuditResync` support) is caught
at the earlier `Hello` probe with typed
`StartError::BrokerProtocolTooOld` (mapped per §3.5.9). The daemon
refuses to mount rather than degrade silently.

##### Lag handling

If the daemon emits records faster than the pusher can drain (a
`cargo build` burst on a flaky broker connection is the typical
case), the bounded broadcast ring overflows. The pusher receives a
`RecvError::Lagged` event carrying
`(dropped_count, oldest_dropped_seq, newest_dropped_seq)` and
emits a chain-stamped `audit_subscribe_lagged` record into the
audit log via a no-broadcast writer (a `Weak`-backed handle that
can't recurse), rate-limited to one event per second. The chain
witness is HMAC-keyed and forge-evident; it appears in the operator
pane via the next successful AuditPush.

##### What pi-rs does NOT get from the audit-ping → AuditPusher swap

The old audit-ping `mode = "fail-closed"` knob blocked write-class
FUSE ops with `EIO` when the per-mount channel was saturated. The
AuditPusher has **no equivalent gate** — by design. The local
audit chain is the source of truth for forensic claims; broker
push is best-effort replication. RFD-0024's threat model treats
agent-blocking on broker liveness as a worse failure mode than
bounded audit-replication lag. Operators who want
broker-roundtrip-or-bust on every write build a watchdog on
`broker.log`'s `audit_pusher_transport_dropped` events plus the
operator HTTP force-close endpoint (§3.5.6) for incident response.

##### References

- contextfs RFD-0024 §"Refactor scope" items 1–6 (audit broadcast
  primitive, pusher loop, replay-state table, sentinel signing,
  AuditResync handshake, Hello probe).
- `crates/contextfs-core/src/audit_broadcast.rs` —
  single-subscriber bounded broadcast.
- `crates/contextfsd/src/audit_pusher.rs` — daemon-side batcher.
- `crates/contextfs-broker/src/audit_replay.rs` — 5-row replay
  acceptance table.
- `crates/contextfs-core/src/instance_close.rs` — sentinel HMAC
  (see §3.5.10 for the lifecycle).
- contextfs memory entry
  `reference_rfd0024_broker_tenant_secret.md` — bring-up gotcha
  (broker MUST share the daemon's tenant secret).
