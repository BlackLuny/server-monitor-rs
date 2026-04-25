# Release process

End-to-end flow for cutting a new server-monitor-rs release and rolling
it out to the fleet.

## 1. Tag and publish

The release pipeline is fully tag-driven. Pushing a `vX.Y.Z` tag triggers
`.github/workflows/release.yml`, which:

1. Builds the SvelteKit frontend once on `ubuntu-latest`.
2. Cross-compiles each binary for every supported target via
   `cargo xtask package`:
   - `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`
   - `x86_64-apple-darwin`, `aarch64-apple-darwin`
   - `x86_64-pc-windows-msvc` (agent + supervisor only)
3. Uploads per-runner archives, aggregates a single `SHA256SUMS`, and
   attaches a Sigstore-keyless build-provenance attestation to each
   archive.
4. Creates / updates the GitHub Release.

```sh
# pre-flight: workspace must be clean and tests green
cargo xtask ci
git status

# tag + push
git tag v0.2.0
git push origin v0.2.0

# watch the run
gh run watch --exit-status
```

A successful run exposes 14 archives (10 panel/agent/supervisor + 4
windows) plus `SHA256SUMS` on the Release page.

### Verifying a release locally

Anyone can verify an artefact came from this exact CI run:

```sh
# Pin the version and fetch the archive + checksums.
gh release download v0.2.0 --repo BlackLuny/server-monitor-rs \
    -p 'monitor-agent-x86_64-unknown-linux-musl.tar.gz' -p SHA256SUMS

# Hash check.
shasum -a 256 -c SHA256SUMS --ignore-missing

# Sigstore attestation.
gh attestation verify monitor-agent-x86_64-unknown-linux-musl.tar.gz \
    --repo BlackLuny/server-monitor-rs
```

The attestation cert names the workflow + git sha; the signer URI starts
with `https://github.com/BlackLuny/…/release.yml@refs/tags/v0.2.0`.

## 2. Roll out to the fleet

Once the tag is published, the panel's release poller picks it up within
five minutes (it caches the latest release in `settings.latest_release`
along with the parsed `SHA256SUMS`). After that, an admin can drive the
rollout from `/settings/updates`:

1. Open the Updates page; confirm `latest_release.tag` matches the new
   version.
2. Pick a percent (slider) — start with `10–25%` for canaries.
3. Optionally restrict to specific agents (multi-select). Combined with
   percent, the panel picks `ceil(percent × |agent_ids|)` agents.
4. Add a short note (visible in the rollout history).
5. Click **Start rollout**. The panel:
   - Inserts `update_rollouts` row in `state='active'`.
   - Materialises one `update_assignments` row per chosen agent with the
     target triple, asset URL, and sha256.
   - Pushes `PanelToAgent::UpdateAgent` to every connected agent in the
     set. Agents that come online later pick up their assignment on
     reconnect.
6. Watch the progress bar fill as agents register with the new version.
   Anything stuck in `pending` past your operational window is your cue
   to pause and investigate.

Pause / resume / abort are one-click on each rollout row. Aborts are
terminal — they don't fire `UpdateAbort` to the agent (M7.1 will), but
no further `UpdateAgent` commands are issued for assignments still in
`pending`.

## 3. Per-agent execution

When an agent receives `UpdateAgent` it forwards the request over its
unix socket (Linux/macOS) or named pipe (Windows) to the supervisor —
the agent never replaces its own binary. The supervisor:

1. Streams the asset to `versions/<v>.partial/`.
2. Verifies the sha256 against the value the panel pushed (from
   `SHA256SUMS`).
3. Extracts to `versions/<v>/`.
4. Updates `state.json`: `staging = <v>`.
5. Stops the agent, rewrites the symlink to point at the new version,
   restarts.
6. Watchdogs the new agent for `grace_s` seconds (default 60s). If the
   process exits inside that window the supervisor reverts to
   `last_known_good`, marks the version in `failed_versions`, and
   continues serving the previous build.

The next time the new agent reaches the panel, its `Register` call
reports `agent_version=<v>`. The panel correlates that with active
assignments and flips matching rows to `succeeded`. When every
assignment for a rollout is terminal (`succeeded` / `failed`), the
rollout itself transitions to `completed`.

## 4. Rolling back

There are two flavours of rollback:

- **Automatic, per-agent.** The supervisor's grace window catches a
  crash-on-start. No admin action needed.
- **Manual, fleet-wide.** Tag a previous version (e.g. `v0.2.0`) again
  is _not_ supported — releases are immutable. Instead, create a new
  rollout pointing at the previous version (`v0.1.0`):

  ```sh
  # If the panel poller doesn't show the older release as "latest",
  # update settings.update_repo / update_channel temporarily, or pin
  # latest by republishing.
  ```

  In M7.1 we'll add a "rollback" button that picks the last shipped
  release as the rollout target. Today, set the version manually in
  the form and start a 100% rollout.

## 5. Operational checklist

Before tagging:

- [ ] `cargo xtask ci` passes locally.
- [ ] Frontend builds without warnings (`pnpm check && pnpm build`).
- [ ] Release notes drafted in `docs/CHANGELOG.md` for the new tag.
- [ ] Migrations run forwards on a fresh schema (`cargo xtask db reset
      && cargo run -p monitor-panel` boots cleanly).
- [ ] Panel binary embeds the right frontend (rust-embed warning if
      `frontend/build/` is missing, so this fires loudly).

After publishing:

- [ ] `gh release view <tag>` shows 14 archives + SHA256SUMS.
- [ ] `gh attestation verify` works on a sample archive.
- [ ] Panel `/settings/updates` shows the new tag within 5 minutes.
- [ ] Canary rollout (10–25%) is green before you ramp to 100%.
