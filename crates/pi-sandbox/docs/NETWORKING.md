# pi-sandbox networking — Linux/Firecracker

How `NetworkPolicy::Allow` works, what the host has to provide, and how
pi auto-installs the pieces that are not standard. See RFD 0023 §2.1.6
for the design rationale; this doc is the operator-facing companion.

## tl;dr

- v1 default is `NetworkPolicy::Deny` — guests boot with no network. Fast,
  simple, no host-side state. This is what `firecracker_smoke.rs`,
  `firecracker_workload.rs`, and the dogfood path use.
- `NetworkPolicy::Allow { ... }` opts the guest in to a single managed
  TAP. Outbound traffic is filtered by host-side nftables in an
  unprivileged user+net namespace; inbound is dropped. There is no
  privileged setup — pasta + nft live in the test/runtime process's own
  userns.
- When the embedder enables network for a sandbox session, pi runs the
  detect-and-install probe described in [Auto-install](#auto-install)
  below. The user gets one prompt explaining what's missing; on accept,
  pi runs the system package-manager command. No silent installs.

## Architecture

```
┌─ pi process (host netns: WAN-attached) ─────────────────────────────┐
│                                                                     │
│  pi sandbox doctor / acquire(spec.network_policy = Allow{…})        │
│   │                                                                 │
│   ▼                                                                 │
│  fork child:                                                        │
│   ├─ unshare -rUn  ← user+net namespace (no host root needed)       │
│   │                                                                 │
│   ▼ inside child netns ──────────────────────────────────────────┐  │
│   │                                                              │  │
│   │  pasta --config-net          ← userspace TCP/UDP relay       │  │
│   │   provides eth-pasta with the host's effective routes.       │  │
│   │   Any L4 traffic from inside the netns goes through the      │  │
│   │   pasta process, which re-emits it on the host's real        │  │
│   │   interface using its own privileges (= caller's privileges).│  │
│   │                                                              │  │
│   │  ip tuntap add tap-pi0 mode tap                              │  │
│   │  ip addr add 172.16.0.1/30 dev tap-pi0                       │  │
│   │  ip link set tap-pi0 up                                      │  │
│   │                                                              │  │
│   │  nft add table ip pi-fw                                      │  │
│   │  nft add chain ip pi-fw forward { policy drop; }             │  │
│   │  nft add rule  ip pi-fw forward iifname tap-pi0 \            │  │
│   │      ip daddr <allowlist> accept                             │  │
│   │  nft add table ip pi-nat                                     │  │
│   │  nft add chain ip pi-nat post { type nat hook postrouting }  │  │
│   │  nft add rule  ip pi-nat post oifname eth-pasta \            │  │
│   │      ip saddr 172.16.0.0/30 masquerade                       │  │
│   │                                                              │  │
│   │  exec firecracker --api-sock …                               │  │
│   │   network-interfaces[0].host_dev_name = "tap-pi0"            │  │
│   │   network-interfaces[0].guest_mac = derived from CID         │  │
│   │   boot_args += " pi.net.ip=172.16.0.2/30"                    │  │
│   │                "  pi.net.gw=172.16.0.1"                      │  │
│   │                "  pi.net.dns=<allowlist[0..N]>"              │  │
│   │                                                              │  │
│   │  guest /init parses pi.net.* off /proc/cmdline and brings    │  │
│   │  up eth0 with the static config. /etc/resolv.conf is         │  │
│   │  written from pi.net.dns.                                    │  │
│   │                                                              │  │
│   └──────────────────────────────────────────────────────────────┘  │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

The whole network plane lives in a child user+net namespace owned by
the running pi process. When the child exits (last VM in this acquire
batch released, or pi crashes), the kernel reaps the netns and every
TAP, nft table, and pasta socket inside it disappears. There is no
host-state cleanup to forget — the failure mode is contained.

## Host requirements

| Tool       | Why                                  | Min version | Notes                                        |
|------------|--------------------------------------|-------------|----------------------------------------------|
| `pasta`    | userspace TCP/UDP relay netns ↔ host | passt 2024  | Ships in `passt` package on most distros     |
| `nft`      | filter / NAT inside netns            | nftables 1  | Built into modern kernels; userspace utility |
| `ip`       | TAP creation + addr config           | iproute2    | Always present                               |
| Linux 5.10+| user+net namespaces, unprivileged TAP| 5.10        | Required for the unshare path                |
| `firecracker` | the VM itself                     | 1.5+        | Same binary as `Deny`-mode                   |

All of these are unprivileged operations. No `sudo`, no host-side TAP,
no host nftables changes, no `/etc/sysctl` writes. Forwarding inside
the child netns is enabled per-namespace (writing `1` to
`/proc/sys/net/ipv4/ip_forward` from inside the unshared netns is a
namespace-local change, not a host change).

## Auto-install

`pi sandbox doctor` performs all of these checks and tells the user
exactly what's missing and how to install it. The check ladder:

1. `which pasta` — `passt` package
2. `which nft` — `nftables` package
3. `unshare -rUn /bin/true` succeeds — kernel allows unprivileged userns
4. `unshare -rUn ip tuntap add tap-doctor mode tap` succeeds — TAP works
5. `unshare -rUn nft add table ip pi-doctor` succeeds — nft works in userns

When a check fails, doctor prints:

```
✗ pasta not found
  Install:
    Debian/Ubuntu:  sudo apt install passt
    Fedora/RHEL:    sudo dnf install passt
    Arch/Manjaro:   sudo pacman -S passt
    Alpine:         apk add passt
  Doc: https://passt.top/passt/about/
```

`pi sandbox setup --net` is the same checks but offers to run the
detected install command interactively (one `[y/N]` prompt per missing
package). It never runs commands without user consent and it never
auto-modifies system files outside the package manager's purview.

When `NetworkPolicy::Allow` is requested at runtime and the host fails
the check ladder, `acquire()` returns `SandboxError::Provider("network
policy 'allow' requires host setup; run `pi sandbox doctor` for
remediation steps")`. We do not auto-install at session-start time —
that would surprise the user with a sudo prompt mid-tool-call.

## Auto-wire on feature enable

The "automatically installed when we have the feature enabled"
guarantee is implemented as:

- **Library entry-point** (`pi-sandbox` crate): the
  `FirecrackerLauncher::acquire()` codepath, when `spec.network_policy`
  is `Allow`, runs the host probe synchronously before forking the
  child userns. Probe failure → fail-fast `SandboxError::Provider`
  (above). Probe success → unshare → set up TAP + nft + pasta in the
  child as described in [Architecture](#architecture).
- **CLI entry-point** (`pi sandbox doctor`): same probe, exposed as
  user-runnable. When the embedder's pi config sets
  `[sandbox.network] enabled = true`, doctor runs as part of `pi`
  startup the first time the binary is invoked after that config
  change, prints the result once, and saves a marker in
  `~/.cache/pi/sandbox-net-doctor-marker.json` so it doesn't
  re-probe on every invocation.
- **Setup entry-point** (`pi sandbox setup --net`): same probe with
  install prompts. Idempotent; safe to re-run.

The library entry-point is the load-bearing one — even an embedder
that ignores `pi sandbox doctor` and goes straight to `acquire()` gets
the diagnostic, just at a slightly later point.

## Two layers of policy gating

There are two cooperating gates between an agent and the open network:

### Layer 1 — `auto_approve` / Cedar policy file (host-side, plan-time)

Decides **whether the agent is allowed to run the tool call at all**.
Lives in `~/.pi/agent/auto-approve.json` (or the embedder's Cedar
policy). The agent runtime consults this gate *before* any sandbox
dispatch — so a policy that rejects `bash apk add cargo` never
reaches the netns at all. Approves / rejects by tool name + input
patterns (regex on `bash` commands, glob on `write` paths, etc.).

This is the existing `auto_approve::Policy` in
`crates/pi-coding-agent/src/auto_approve/`. No change in this
direction — it already gates `bash` calls, and `apk add …` is just
a bash invocation.

### Layer 2 — `NetworkPolicy::Allow.egress_allowlist` (host-side, kernel-time)

Decides **which destinations the guest can reach**, even if Layer 1
already allowed the tool to run. Encoded as nft rules in the
unprivileged child userns. Set by the operator's pi config:

```toml
[sandbox.network]
enabled = true
allowlist = [
  # apk + crates.io + GitHub (the minimum to compile a real Rust
  # crate inside the guest):
  "dl-cdn.alpinelinux.org",
  "crates.io",
  "static.crates.io",
  "index.crates.io",
  "github.com",
  "objects.githubusercontent.com",
]
```

Each entry is a hostname, a literal IPv4 address, or an IPv4 CIDR.
Hostnames are resolved to IPs **inside the netns at setup time**
(via pasta's userspace DNS forwarder); the resulting IPs become the
nft accept set. UDP/53 + TCP/53 to the configured `guest_dns`
resolvers is always permitted regardless of the list — without it
the resolution itself would deadlock.

The forward chain default is `drop`. ESTABLISHED,RELATED state is
allowed (so reply packets get back to the guest). Everything else is
silently dropped: the guest sees timeouts, not "connection refused"
— deliberate, so a misbehaving agent can't probe what's reachable.

**Empty list = closed network** (defense in depth). The launcher
only enforces what the policy file declares; it doesn't invent
permissions.

### How they cooperate

| What                                  | Layer 1 (auto_approve)  | Layer 2 (egress allowlist) |
|---------------------------------------|-------------------------|----------------------------|
| Decides on input?                     | Yes (tool name + args)  | No (destinations only)     |
| Sees the bash command being run?      | Yes (regex match)       | No                         |
| Sees the destination IP/host?         | No                      | Yes                        |
| Enforced where?                       | Runtime (pi-rs process) | Kernel (netns nft chain)   |
| Bypassable from inside the guest?     | N/A (never reached)     | No (nft is below userspace) |
| Failure mode on deny                  | Tool call refused       | Connection times out       |

Both layers are needed. Layer 1 alone misses agents that bypass tools
(e.g. shell-out to `nc`); Layer 2 alone allows arbitrary tools to run
as long as they only reach approved hosts.

### Future: per-connection broker (Cedar PDP, dynamic egress)

For policies more dynamic than a static IP set (e.g. "let the guest
talk to whatever URL the LLM names, but only after Cedar approves
that specific destination"), the natural extension is a
per-connection broker: a UNIX socket inside the netns
(`/run/pi-bridge/net.sock`) where the guest asks
`{ host, port, sni }` and the host's Cedar PDP returns
allow / deny. On allow, the host JIT-inserts an nft rule pinned to
that 5-tuple; the rule expires on connection close.

This is **not** v1. The current static allowlist + auto_approve is
the v1 surface. The broker is RFD-grade work — RFD 0023 §"selective
network egress" follow-up. The infrastructure here is ready for it:
the netns, pasta, nft chains, and rule generation already exist;
the broker just replaces "render N rules at setup" with "render 1
rule per accepted connect()."

## Observability — full egress trace

The launcher exposes pasta's two built-in tracing knobs through one
env var:

```sh
export PI_SANDBOX_FC_PCAP_DIR=/var/log/pi/sandbox-pcap
```

When set, every `NetworkPolicy::Allow` acquire writes:

| File                              | Format    | Content                                                              |
|-----------------------------------|-----------|----------------------------------------------------------------------|
| `<vm_id>.pcap`                    | libpcap   | Full L2 capture of every frame on the netns-side of pasta            |
| `<vm_id>.pasta.log`               | text      | pasta's own forwarding decisions (connection setup / teardown)       |

The pcap is openable in `tcpdump -nn -r <file>` or wireshark; it
includes every DNS query, every TCP connection, every UDP datagram
the guest emitted. For the `apk add cargo` demo the pcap is
~330 MB (proportional to the 888 MiB download).

**Privacy note:** pcap captures full payloads. TLS-protected payloads
are still encrypted, but the SNI + IP destinations are visible — that
is the audit data. Do not enable `PI_SANDBOX_FC_PCAP_DIR` on shared
hosts without a clear retention policy.

**Attribution to tool call:** the pcap is per-VM, not per-call. To
attribute a connection to a specific tool invocation, join the
pcap timestamps against the worker's per-call boundaries logged on
the host side (`tracing` events from `pi_sandbox::microvm::firecracker`,
filterable on `vm_id`). A future commit will surface this as a
`pi sandbox trace <vm_id>` subcommand that produces the join
automatically.

## Troubleshooting

| Symptom                                                    | Cause                                            | Fix                                                                  |
|------------------------------------------------------------|--------------------------------------------------|----------------------------------------------------------------------|
| `acquire()` returns `Provider("network policy 'allow'…")`  | Host probe failed                                | `pi sandbox doctor`                                                  |
| Guest boots but `apk add` hangs at "fetching"              | DNS unreachable from guest                       | Verify `pi.net.dns` IPs in `/proc/cmdline` resolve from inside netns |
| Guest boots, DNS works, but `apk add` says "host unreachable" | Allowlist IPs went stale (DNS rotation)       | Restart the netns (release+reacquire); IPs re-resolve at startup     |
| `apk add` works for crates.io but `cargo build` 404s on `objects.githubusercontent.com` | Allowlist missed transitive host | Add the missing host to `[sandbox.network] allowlist`               |
| `unshare -rUn /bin/true` fails with `Operation not permitted` on Debian/Ubuntu | sysctl `kernel.unprivileged_userns_clone=0`  | `sudo sysctl -w kernel.unprivileged_userns_clone=1`; persist via `/etc/sysctl.d/`                  |

## Vsock-proxied tools and `NetworkPolicy`

Some tools (currently `web_search`, more arriving) need *host* network
access — to call `api.exa.ai`, the OpenAI API, etc. They run on the
host (so the upstream API key never enters the guest) and are
proxied to the agent over a per-VM vsock UDS at `<vsock_path>_5003`.

Vsock is a kernel channel parallel to eth0/TAP/nft. Without explicit
gating it would silently bypass `NetworkPolicy::Deny` — the operator
says "no network" but `web_search` works anyway. We close that
bypass by tying the proxy listener's bind to the policy:

| Policy                         | eth0 egress                                                  | `web_search` proxy                                               |
|--------------------------------|--------------------------------------------------------------|------------------------------------------------------------------|
| `Deny`                         | not configured                                               | listener never binds → guest gets `vsock: Connection reset`      |
| `Allow { egress_allowlist: [] }` | TAP/nft/pasta wired but `policy drop` for new flows        | listener bound → request runs on host with host AuthStorage      |
| `Allow { egress_allowlist: [...] }` | TAP/nft/pasta + accept rules for the resolved IPs    | listener bound → request runs on host                            |

So `NetworkPolicy::Allow` is now the single switch the operator
flips for "this VM may use the network" — covering both the eth0
plane and the vsock-proxy plane. There is no path for an agent to
do network I/O under `Deny`.

Future per-tool refinement (v1.1+): a `[sandbox.tools.<name>]
enabled = bool` config so the operator can turn individual proxied
tools on/off independently of the broader `Allow` switch.
Implemented in `cold_boot()` as `matches!(spec.network_policy,
NetworkPolicy::Allow { .. })`; regression-tested in
`tests/firecracker_web_search_proxy.rs::web_search_blocked_under_network_policy_deny`.

## Per-call hygiene (RFD 0023 §"Post-call hygiene")

Between every tool call on the same warm VM, the worker wipes
writable scratch paths so files written in call N aren't visible
in call N+1:

| Path        | Wiped between calls? | Why                                                |
|-------------|----------------------|----------------------------------------------------|
| `/tmp`      | ✅ yes               | Conventional process scratch                       |
| `/var/tmp`  | ✅ yes               | Long-lived scratch (still treated as scratch here) |
| `/root`     | ✅ yes               | Default home for tools that write `~/.config` etc. |
| `/etc`, `/usr`, `/opt`, `/home/*` | ❌ no | Persist within the VM lifetime (overlay upper). Pool retirement (`MAX_CALLS=50`, `MAX_AGE=5min`) caps the blast radius. |

The bash tool is the only one that writes scratch state today; the
read/write/edit/grep/ls/find tools either hit `host_cwd` (gone in
v1 because virtio-fs is dropped — see §"Filesystem semantics" of
RFD 0023) or are read-only. So the practical effect is: each
`bash` call gets a freshly empty `/tmp`.

**What this does NOT cover** — the non-scratch overlay upper
(writes to `/etc`, `/usr`, `/opt`, etc.) still persists across
calls within ONE VM lifetime. There are two ways to bound that:

1. **`PI_SANDBOX_FC_MAX_CALLS=1`** (works today) — every tool
   call cold-boots a fresh VM. ~1s overhead per call but every
   call gets a guaranteed-pristine overlay upper, no leftover
   processes, no stale routing/nft state. Default is 50 (warm
   pool reuse for performance).

2. **`pi-vm-reset` agent** (v1.1, RFD 0023 §"Post-call hygiene")
   — same logical reset in ~50ms via overlay re-mount with
   `move_mount` survival list + `pivot_root` into a fresh
   upper. Not yet implemented. The MAX_CALLS=1 knob is the
   simple "destroy the VM" alternative until that lands.

For the default 50-call window, the warm-pool retirement cap is
the outer hygiene boundary.

Verified by `tests/firecracker_per_call_hygiene.rs::tmp_is_wiped_between_tool_calls_in_same_vm`.

## What this does NOT do

- **No virtio-net device hot-plug.** Allow vs Deny is a boot-time
  property of the VM, just like rootfs version and vCPU count. Two
  acquires with different network policies share no warm pool — they
  reach different cold-boot paths. This is correct: a warm VM that
  was started under Deny has no kernel-side eth0 to suddenly get
  packets on.
- **No host-side iptables/nftables rules.** All filtering is
  inside-netns. The host's firewall is untouched.
- **No transparent DNS rewriting.** The guest's resolver is exactly
  the IPs the host put in `pi.net.dns`. If those IPs change between
  the time the host wrote them and the time the guest queries, the
  guest gets the new answer (because pasta forwards UDP/53 to the
  host's resolver) — but the allowlist is still pinned to the
  setup-time A records. Mismatches surface as "host unreachable" not
  "DNS lies".
- **No cross-netns sharing.** Each `acquire()` with `Allow` gets its
  own child user+net namespace. Two concurrent acquires can't see
  each other's TAPs. This is the simplest model and matches what
  pi-sandbox already does for the warm pool's per-VM filesystem
  overlays.
