# Releasing pi-sdk

Per RFD 0027 §6 + Commit J. This is the procedure the maintainer
follows to publish a `pi-sdk` MINOR to crates.io.

## Prerequisites (one-time, before the first release)

1. **Crate name availability check.** Confirm none of the names
   below are already taken on crates.io. If `pi-sdk` is taken, the
   RFD's fallback is `pirs-sdk` or `pi-rs-sdk` (Open Question #8).
   ```bash
   for name in pi-tool-types pi-ai pi-tools-core pi-tools-net \
               pi-tools pi-sandbox-protocol pi-sandbox \
               pi-agent-core pi-sdk; do
     if cargo search "${name}" --limit 1 2>/dev/null | grep -q "^${name} = "; then
       echo "TAKEN: ${name}"
     else
       echo "AVAILABLE: ${name}"
     fi
   done
   ```

2. **License decision** (RFD 0027 Open Question #7). Workspace
   currently declares `license = "MIT"`. The RFD recommends
   MIT/Apache-2.0 dual-license (Rust-ecosystem norm). If switching
   to dual:
   ```toml
   # Cargo.toml [workspace.package]
   license = "MIT OR Apache-2.0"
   ```
   And ship `LICENSE-MIT` + `LICENSE-APACHE` files at the repo root.
   Defer to the maintainer's final call before the first publish.

3. **crates.io API token.**
   ```bash
   cargo login <api-token>
   ```
   The token must have `publish-new` + `publish-update` scopes.

4. **GitHub Security Advisories enabled** on the pi-rs repo (per
   `SECURITY.md`).

5. **RUSTSEC namespace reservation.** Open a PR against
   https://github.com/rustsec/advisory-db reserving the
   `RUSTSEC-2026-PI-SDK-NNNN` namespace (per `SECURITY.md`).

## Publish order

`pi-sdk` depends on every other publishable workspace crate. They
must be published in dependency order, lowest first. Publishing
out-of-order causes `cargo publish` to fail with "package not
found in registry."

```
1.  pi-tool-types          (no workspace deps)
2.  pi-ai                  (depends on pi-tool-types)
3.  pi-tools-core          (depends on pi-tool-types)
4.  pi-tools-net           (depends on pi-tool-types, pi-tools-core)
5.  pi-tools               (depends on pi-tools-core, pi-tools-net, pi-ai)
6.  pi-sandbox-protocol    (no workspace deps)
7.  pi-sandbox             (depends on pi-tools, pi-tool-types, pi-sandbox-protocol)
8.  pi-agent-core          (depends on pi-ai, pi-tools, pi-sandbox)
9.  pi-sdk                 (depends on all of the above)
```

NOT published (all marked `publish = false`):
- `pi-coding-agent` — the `pi` binary; embedders use `pi-sdk`.
- `pi-tui` — binary-side TUI rendering.
- `pi-stats` — binary-side ingest + dashboard.
- `pi-orchestrate` — binary-side campaign runner.
- `pi-sandbox-worker` — guest-side binary, distributed via the
  rootfs artifact, not crates.io.
- `pi-sandbox-rootfs` — workspace-internal scaffolding for the
  rootfs build recipe (`build.sh`); `ROOTFS_VERSION` was inlined
  into `pi-sandbox/src/microvm/types.rs` so `pi-sandbox` is a
  publishable leaf (per pass-6 finding #1).
- `pi-sdk-canary` — test crate.

## Per-release checklist

Before tagging:

- [ ] All Track-1 + Track-2 + Track-3 commits merged to `main`.
- [ ] CI workflow `pi-sdk-supply-chain.yml` green on the latest
      commit (audit + deny + canary + examples + doctests +
      matrix-up-to-date).
- [ ] `cargo test -p pi-sdk --features mocks` — all 19 unit + 3
      doctests pass.
- [ ] `cargo test -p pi-sdk-canary` — all 10 unit + 1 integration
      pass.
- [ ] `cargo build --examples -p pi-sdk --features mocks` clean.
- [ ] `bash scripts/gen-compatibility-matrix.sh` produces no diff
      (markdown in sync with TOML).
- [ ] `crates/pi-sdk/CHANGELOG.md` `[Unreleased]` block frozen to
      the new version + datestamp; new `[Unreleased]` block opened.
- [ ] `compatibility.toml` has a row for the new release.

Dry-run each crate (in the order above):

```bash
for name in pi-tool-types pi-ai pi-tools-core pi-tools-net \
            pi-tools pi-sandbox-protocol pi-sandbox \
            pi-agent-core pi-sdk; do
  echo "==> dry-run: ${name}"
  cargo publish --dry-run -p "${name}" || { echo "FAIL: ${name}"; exit 1; }
done
```

(Note: each `--dry-run` after the first will pull the previously-
dry-run-uploaded crate from crates.io, but the dry-run itself does
NOT actually upload. So later crates in the order will still report
"package not found" until the real publish has run.)

Real publish:

```bash
for name in pi-tool-types pi-ai pi-tools-core pi-tools-net \
            pi-tools pi-sandbox-protocol pi-sandbox \
            pi-agent-core pi-sdk; do
  echo "==> publish: ${name}"
  cargo publish -p "${name}" || { echo "FAIL: ${name}"; exit 1; }
  # crates.io rate-limits — pause 30s between uploads.
  sleep 30
done
```

After publish:

- [ ] Tag the release in git: `git tag -a pi-sdk-v0.1.0 -m "pi-sdk 0.1.0"`
      + `git push --tags`.
- [ ] Re-enable the `semver-checks` job in
      `.github/workflows/pi-sdk-supply-chain.yml` (set `if: false`
      → remove the line). Open a PR.
- [ ] Verify docs.rs has built `pi-sdk` successfully:
      https://docs.rs/pi-sdk/0.1.0
- [ ] Announce release on GitHub Discussions + relevant chat
      channels.
- [ ] Update the `compatibility.toml` matrix-canary CI step to
      pin to 0.1.0 (the old "previous MINOR" was a placeholder
      since no published version existed).

## Yanking a release

Per RFD 0027 §3: yank a release ONLY when a security vulnerability
is published in the same release. Otherwise prefer publishing a
PATCH release with the fix.

```bash
cargo yank --version 0.1.0 -p pi-sdk
```

Yanks are reversible (`cargo yank --undo`); the published code
remains downloadable, only `cargo update` skips the yanked version.

## When the publish fails partway

If publish fails after some crates have shipped but not all:

1. Don't `cargo yank` the partial uploads — they're real releases
   that other embedders may already have downloaded.
2. Fix the underlying issue in a follow-up commit + bump the version
   of any unshipped crate to `0.1.1` (or higher) so the partial
   release stays consistent.
3. Restart the publish loop from the failing crate, with the new
   version.

## Out of scope

- Multi-target binary releases (musl, macOS, Windows) — that's
  pi-coding-agent's job, not pi-sdk's.
- Compiled-agents distribution (RFD 0028) — future RFD.
- Mirror to alternative registries (lib.rs / sourcehut) — happens
  automatically.
