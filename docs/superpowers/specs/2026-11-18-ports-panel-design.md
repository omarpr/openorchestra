# Ports panel and Tailscale sharing — design

Date: 2026-11-18. Status: approved 2026-11-18.

## Why

Two needs, one feature:

1. **Webhooks into the apps agents build.** An agent runs a dev server
   (`:3000`) in a task's worktree. Stripe, GitHub and similar need a stable,
   public HTTPS URL to call it. Today that means running ngrok by hand.
2. **Remote control away from home.** The phone UI (`:7443`) only works on
   the home network. It should also work from anywhere, but only for the
   owner's own devices.

Webhooks that *trigger agents* (into openOrchestra itself) are out of scope:
that is a separate project with its own security model.

## Goals

- A **Ports** tab in the right-hand tools column (beside Files, Git and
  Review) listing listening TCP ports, with Open in browser and Copy URL.
- Share a port through Tailscale: **private** (`tailscale serve`, tailnet
  only) or **public** (`tailscale funnel`, internet), with a stable URL
  (`https://<mac>.<tailnet>.ts.net[:port]`).
- Remote control reachable over the tailnet, directly, keeping our own
  certificate authority and the peer check meaningful.
- Tailscale is **optional**: shown on the first-run requirements screen as an
  Optional row, never required. Without it, the Ports tab still lists and
  opens ports. Sharing and tailnet remote control explain how to get it.

## Non-goals

- Triggering agents from webhooks.
- Cloudflare in this round. Keep the sharing code behind a small provider
  boundary so Cloudflare can be added later, but don't build an abstraction
  beyond what Tailscale needs.
- Embedding Tailscale (tsnet is Go-only) or talking to its local API.
- UDP, and ports bound to a specific non-loopback interface for some other
  purpose. TCP listeners only.

## Anti-goals (a failure even if it works)

- Anything becomes public without an explicit, per-port action by the user.
- A public share outlives the thing it was for (task archived, port closed,
  app quit) without the user knowing.
- Remote control traffic arrives as 127.0.0.1 through a proxy, so the peer
  check passes everything. That is why remote control does **not** go through
  `tailscale serve`.
- The Ports tab costs noticeable CPU when nobody is looking at it.

## Constraints found in the code

- Tools tabs: `ToolTab` (`crates/harness-app/src/workspace/prefs.rs`) and
  `render_tool_body` / `render_tool_tab_strip`
  (`crates/harness-app/src/workspace/ui/tool_panel.rs`).
- Process-to-worktree attribution already exists in
  `crates/harness-app/src/resource_monitor.rs`: tmux sessions are matched to a
  worktree by `tmux::session_prefix`, and their pane PIDs are walked down the
  process tree (`aggregate`). The sampler runs in
  `workspace/construct.rs` (~line 612).
- First-run checks: `crates/harness-app/src/preflight.rs`, `Severity::Optional`.
- Remote control: `crates/remote`. The peer filter (`netfilter.rs`) rejects
  `100.64.0.0/10`, Tailscale's IPv4 range. Tailscale's IPv6 range
  (`fd7a:115c:a1e0::/48`) is ULA and already allowed by the filter. There is
  also a per-network trust model (`network.rs::peer_trusted`, trusted network
  keys in Settings → Remote).
- Our CA carries **name constraints** (`certs.rs::ca_name_constraints`): the
  hostnames/IPs it may sign for are baked into the CA. Adding the tailnet name
  means a new CA (`retire_ca`), so already-paired phones must re-install the
  profile once. The UI must say so before doing it.
- Measured on a dev Mac: `lsof -nP -iTCP -sTCP:LISTEN -F pcn` takes ~60 ms for
  the whole machine. Listeners seen include system ones (ControlCenter :5000
  and :7000 for AirPlay, rapportd), the agent CLIs' own internal ports (claude
  listens on 127.0.0.1 for its IDE bridge), OrbStack/Docker, LM Studio, ollama.
- Two `tailscale` CLIs can coexist (Homebrew on PATH and
  `/Applications/Tailscale.app/Contents/MacOS/Tailscale`), with a client and
  daemon version-mismatch warning on stderr. Parse stdout JSON only.

## Approaches considered

- **A. `tailscale` CLI + `lsof` for ports (chosen).** Port detection from one
  `lsof` call, joined to the existing process-tree attribution. Sharing via
  `tailscale serve --bg` / `tailscale funnel --bg`, state from
  `tailscale status --json` and `tailscale serve status --json`. Remote
  control over the tailnet directly. No new crates. Depends on CLI behaviour.
- B. Like A, but remote control behind `tailscale serve`. Less certificate
  work, but every connection arrives from loopback. Rejected (anti-goal).
- C. Tailscale local API over its socket. No subprocess, but undocumented and
  different between App Store and standalone builds. Rejected as brittle.

The user chose A.

## Which ports are shown: ours and the outside world

The user asked whether to show only what we run or also outside processes.
Decision (proposed): **both, in two sections.**

1. **This task** (top, open): listeners whose process is in the selected
   task's tmux process trees (same attribution as the footprint monitor). For
   a multi-repo task, all members' trees, labelled by member.
   - Excluded: the agent CLIs' own internal listeners (a listener whose
     process *is* the agent binary for that pane: claude, codex, opencode,
     kimi, pi), because they are not the user's app. Shown only if the user
     ticks "Show agent ports" (not persisted; builder's call whether to add it
     at all).
2. **Other on this Mac** (collapsed by default): every other TCP listener,
   minus openOrchestra itself, the agent CLIs' own listeners (other Claude
   Code sessions alone add several), and processes whose executable is under
   `/System/` or `/usr/libexec/` (AirPlay, rapportd and friends).
   Why keep it: an agent's `docker compose up` shows up under
   OrbStack/Docker, not under the task, and that is exactly the kind of server
   that receives webhooks. Sharing works on these rows too.

**Containers (added 2026-11-18).** A container's published port is held by
the engine (OrbStack, Docker Desktop, Colima…), so the process name says
nothing useful. When an engine process is listening, run `docker ps` (2 s
timeout, active context only) and label those rows with the Docker icon
(zed's `icons/file_icons/docker.svg`), `service · project` (or the container
name) and the port inside the container. A Compose container whose
`com.docker.compose.project.working_dir` is inside a scope's folder is listed
under This task. Otherwise, a container with a bind mount inside a scope's
folder goes there; failing that, one whose name starts with the scope's repo
name (alphanumerics only, case-insensitive, repo name at least 4 characters)
goes there marked "matched by name". A Compose container is never re-homed by
name. No `docker` CLI or a stopped daemon: rows stay plain.

Each row: port, bind address (`*`, `127.0.0.1`, `::1`), process name, PID
(on hover), and its share state.

## Design

### Step 1 — Ports tab (no Tailscale needed)

- `ToolTab::Ports` after Review. Tab label `Ports · N` where N is the count in
  "This task".
- New module `crates/harness-app/src/ports.rs`, pure where possible:
  - `parse_lsof(&str) -> Vec<Listener { pid, comm, addr, port }>` (the `-F
    pcn` format, deduplicating IPv4/IPv6 and multiple fds per port).
  - `classify(listeners, attribution) -> PortsView { task: Vec<Row>,
    other: Vec<Row> }`, reusing the pane-PID → subtree walk from
    `resource_monitor` (factor the subtree walk out so both share it, rather
    than copy it).
- Sampling: on a background thread, every ~3 s, **only while the Ports tab is
  visible** (plus one immediate sample when it becomes visible).
- Actions per row: Open in browser (`http://localhost:<port>`), Copy URL.
- Empty state: "No servers running in this task" plus a short hint.

### Step 2 — Tailscale detection and the requirements row

- `preflight.rs`: add `tailscale` with `Severity::Optional`. Cost: "Ports can't
  be shared and Remote control only works on your home network." Fix:
  "brew install --cask tailscale (or install Tailscale from tailscale.com)".
  The lookup must also try the app-bundle path when it isn't on PATH. This is
  the "show as an option, don't require it" behaviour the user asked for.
- A small `tailscale.rs` wrapper: find the binary; `status()` →
  `{ running, dns_name, ipv4, ipv6 }` from `status --json`; command calls are
  bounded with a timeout, like `tmux::run_bounded`.
- States the UI must handle: not installed, installed but logged out or
  stopped, running.

### Step 3 — Remote control over the tailnet

- Settings → Remote shows the tailnet as a network the user can trust, using
  the existing trusted-networks model. Only when trusted does
  `100.64.0.0/10` pass, and only when the peer address is in Tailscale's
  range **and** the Mac's own tailnet IP is in that range. Don't open
  CGNAT addresses generally: carrier CGNAT uses the same range.
- Add the tailnet DNS name and IPs to the server certificate. Because of the
  CA name constraints, this needs a new CA: confirm first ("Paired phones will
  need to re-install the profile once"), then `retire_ca` and reissue.
- Settings → Remote shows the tailnet URL (`https://<mac>.<tailnet>.ts.net:7443`)
  next to the home-network one, with its own QR.

### Step 4 — Sharing a port

- Per row: **Share ▸ Private (tailnet)** / **Public (internet)**.
  - Private: `tailscale serve --bg --https=<p> http://localhost:<port>`.
  - Public: `tailscale funnel --bg --https=<p> http://localhost:<port>`, after
    a confirmation that names the port and process and says "anyone on the
    internet can reach this".
  - `<p>` is one of 443, 8443, 10000 (Funnel's only ports). Use the first
    free one; if all three are taken, explain and offer to stop one.
    Path-based sharing (`--set-path`) is out of scope: many dev servers break
    under a path prefix.
- A shared row shows a **Private** or **Public** badge and the URL, with Copy
  and Stop. The tab label adds a dot when anything is public.
- Ownership: the app records what it shared in the state dir and only stops
  shares it created. Shares made by the user outside the app are shown
  read-only, from `serve status --json`.
- Stop automatically when the task is archived, when the port stops
  listening for ~10 s, and on app quit. On launch, remove leftovers the app
  recorded but no longer has a reason for, and say so in a note.
- Funnel needs the `funnel` attribute in the tailnet policy. When the CLI
  says it's missing, show that message and a link to Tailscale's Funnel docs.

### Docs

- README: a "Ports and sharing" section. Mention that Tailscale's free plan is
  for non-commercial use (the user's own account, the user's call).
- CHANGELOG.md Unreleased line per user-visible commit.

## Testing

- Unit: `parse_lsof` (real captured output, IPv4/IPv6 duplicates, names with
  spaces like `LM\x20Studio`), `classify` (task vs other, agent ports hidden,
  system processes hidden), port-slot choice, peer check with the tailnet
  range on and off, cleanup decisions.
- Tailscale wrapper: parse captured `status --json` and `serve status --json`
  fixtures; no live Tailscale in tests.
- Manual: a scratch app per the existing visual-check practice, with a
  `python3 -m http.server` in a task terminal; share private, then public;
  curl the URL from outside; archive the task and confirm the share is gone.

## Left to the builder

- Exact sampling interval and the "port gone" grace period.
- Whether "Show agent ports" exists as a toggle.
- Layout details of rows and badges, within the existing tool panel style.
- Where the share records live in the state dir.
