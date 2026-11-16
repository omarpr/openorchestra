# oo-remote — LAN remote control of agents from a phone (design)

**Date:** 2026-11-16 · **Status:** draft for approval · **Scope:** phases 1–3

## Intent

The user leaves the Mac on with agents working and wants to follow their
progress and type into them from a phone on the same home network, without
sitting at the desk. The phone side is an installable PWA (Vite + React) so it
lives on the home screen like an app.

Only agents the user has explicitly **marked for remote control** in
openOrchestra are reachable. Everything else on the Mac — other agents, plain
shells, the filesystem — must stay invisible to the phone.

## Goals

1. Mark/unmark individual agent tabs for remote control from the app.
2. An in-app HTTPS server (on while "Remote control" is enabled in Settings)
   that serves the PWA and an API limited to marked tabs.
3. Phone UI: list of marked agents with live status (running / waiting /
   idle / stopped), waiting agents first; open one to see its live terminal;
   type into it (full keyboard via the terminal + a quick-reply box).
4. Trust and access: the app creates its own local certificate authority
   (chosen: option A), each phone installs/trusts it once; phones pair with a
   one-time code and get a revocable device token.
5. Keep-awake option so the Mac doesn't idle-sleep while remote is enabled.

## Phases (each one usable on a real device)

1. **Watch.** Marking in the app, Remote settings with enable and port,
   in-app server with the CA, server certificate and pairing (needed even to
   watch, because a PWA requires HTTPS), agents list with live status, and a
   read-only live terminal. Responsive PWA shell, installable.
2. **Type.** Keystroke input through control mode, the mobile key bar, the
   quick-reply box, "Fit to phone", and read-only devices.
3. **Harden and stay up.** Device list with revoke, pairing attempt limits,
   private-IP filter and Origin/CSP checks, certificate re-issue when
   addresses change, coordinating Dev and release builds through the file
   (§6), and keep-awake.

## Non-goals (this round)

- Access from outside the LAN; push notifications; diff/review view;
  starting/stopping/creating agents or tabs from the phone; file browsing;
  multiple user accounts. (Push notifications and diff view are a later phase.)
- Remote working while openOrchestra is closed. **Accepted trade-off** of the
  chosen approach (server inside the app).

## Anti-goals (failure even if it "works")

- Any way for a client to reach a tmux session that is not currently marked,
  including by guessing ids or names, or by tmux key bindings/commands.
  (Marking a tab does trust the phone with whatever *that* terminal can do;
  for a plain Shell tab that means a shell. The mark menu says so for Shell
  tabs.)
- Anything that blocks or stutters the GPUI main thread.
- Writing to `panes.json` (Dev and release builds share it and old builds
  clobber it) or otherwise disturbing the desktop session layout.
- A phone attach that resizes or garbles the agent's pane on the desktop by
  default.

## Approaches considered

1. **Standalone `oo-remote` daemon under launchd** — survives the app closing;
   costs a second install artifact and version skew.
2. **Server inside openOrchestra (chosen by the user)** — a background
   service started by the app, like the A2A broker. Nothing extra to install;
   remote works only while the app runs; changes rebuild the app.
3. **In-app, text snapshots (`capture-pane`) instead of a live terminal** —
   simplest, but loses arrow-key menus, Ctrl-C, approval prompts. Rejected:
   "write in it if need be" needs real interaction.

## Current state this builds on (facts from the code)

- Every terminal/agent tab is a tmux session on the user's default tmux
  server, named `tmux::session_name(worktree_key, tab_id)` =
  `harness_<slug>_<tabId>` (`crates/harness-app/src/tmux.rs:29`). Agents
  survive app restarts.
- `tmux::send_keys` (`tmux.rs:415`) types a line + Enter with bounded
  timeouts; the A2A bridge already flattens newlines before using it
  (`workspace/a2a.rs:300`).
- Agent status comes from `<worktree>/.harness/status.json` via the
  `status-watch` crate — **per worktree, not per tab**. The app already holds
  this status in memory and also has the "STOPPED" notion for a dead agent tab.
- State files live in `~/.openorchestra`, written privately/atomically via
  `state_files::write_private` (0700 dir / 0600 files).
- Settings has sections `Appearance, Editor, Diff, Agents, Notifications,
  Shortcuts` (`workspace.rs:48`). The broker is a background-thread server
  started from the app (`workspace/a2a.rs:281`).
- `tokio`, `rustls`, `tokio-rustls`, `tungstenite` are already in
  `crates/harness-app/Cargo.lock` via the zed cone.
- Reminder: build `harness-app` from its own directory; root `cargo build`
  skips it.

## Design

### 1. Marking agents (desktop app)

- **Unit = one tab** (one tmux session). Any terminal tab — `Agent(_)` or
  `Shell` — can be marked; the menu offers it on terminal tabs only, never on
  Editor/Diff/Chat. *(My call: some agents run in tabs recorded as `Shell`,
  e.g. a `claude` tab in today's `panes.json`.)*
- Toggle lives in the tab's right-click menu ("Allow remote control" /
  "Stop remote control"); a marked tab shows a small remote indicator in the
  tab strip and a dot next to its worktree in the sidebar. Default: unmarked.
- Closing/deleting/archiving a tab or worktree unmarks it.
- Unmarking takes effect immediately: open terminal sockets for that session
  are closed, and it disappears from the list.
- Marks persist across restarts in `~/.openorchestra/remote.json` (see §6),
  never in `panes.json`.

### 2. Server lifecycle (inside the app)

- Settings gets a new **Remote** section: enable toggle, port (default
  `7443`), bind status ("Listening on https://<host>.local:7443 —
  192.168.1.20"), phone setup/pairing (§4), paired devices list with revoke,
  keep-awake toggle, CA fingerprint.
- Enabling starts the service on its own thread(s)/tokio runtime, never on the
  GPUI executor. Disabling stops it and closes every connection. The enabled
  state persists; the app restarts the service at launch if enabled.
- The app owns an `Arc<RwLock<RemoteState>>` (marks, devices, live status
  snapshot) that the GUI updates and the server reads; status changes are
  pushed through a broadcast channel to connected clients.
- Port in use (e.g. Dev and release both running) → the section shows the
  error; the app keeps working.
- **Network scope:** listen on all IPv4 interfaces, but accept connections
  only from loopback, RFC 1918, and link-local source addresses; drop others
  at accept. *(My call.)*

### 3. API (all HTTPS; everything except the PWA shell and `/pair` needs a device token)

Clients never send tmux session names. Each marked tab gets an opaque id
(random, stable while marked); the server maps id → session only through the
current marks, so an unmarked or unknown id is a 404.

- `GET /api/agents` → `[{ id, title, agent_kind, project, worktree, status,
  status_since, tool }]` for marked tabs whose tmux session is live, plus
  marked-but-dead ones as `stopped`. Status is the worktree's status
  attributed to its marked tabs.
- `GET /api/events` (SSE) → `agents` snapshot on connect, then
  `status`/`added`/`removed` events.
- `WS /api/term/:id` → live terminal (§5). First client message must be
  `{type:"auth", token}`; otherwise close.
- `POST /api/agents/:id/send {text}` → quick reply: newlines flattened to
  spaces, typed with `send-keys -l` + Enter (reuse `tmux::send_keys`).
  Rejected for read-only devices.
- `POST /api/pair {code, device_name}` → `{token}` (§4).
- Auth via `Authorization: Bearer <token>` for HTTP/SSE (SSE via `fetch`
  streaming, not `EventSource`, so the header can be sent). WebSocket
  upgrades also check `Origin` equals the server's own origin.

### 4. Trust and pairing (option A: app-made local CA)

**Certificates.** On first enable the app generates (e.g. with `rcgen`):
- a CA: ECDSA P-256, CN "openOrchestra Remote CA (<computer name>)", 10-year
  validity, stored in `~/.openorchestra/remote/ca.{crt,key}` (0600);
- a server certificate signed by it with SANs `<LocalHostName>.local`,
  `localhost`, `127.0.0.1`, and each current private IPv4 address; validity
  ≤ 397 days (Apple's limit for TLS server certificates). Re-issued at
  service start and whenever the set of LAN addresses changes (checked
  periodically) or it is within 30 days of expiry.

**Phone setup — step 1, trust (once per phone).** Settings shows a QR code
for `http://<ip>:7080/setup` — a **plain-HTTP** port that serves only the
setup page and the public CA certificate (never the API, never the PWA). The
page detects the platform:
- iOS: downloads a `.mobileconfig` profile with the CA, then tells the user to
  install it in Settings and enable it under General → About → Certificate
  Trust Settings.
- Android: downloads `ca.crt` with instructions to install it as a CA.
The setup page and the desktop Settings both show the CA's SHA-256
fingerprint so the user can compare. The HTTP port runs only while the
Settings → Remote "Add a phone" panel is open. *(My call: limits the
unauthenticated surface.)*

**Phone setup — step 2, install and pair.** The setup page then links to
`https://<LocalHostName>.local:7443/`. From there the user adds the PWA to the
home screen. On first open without a token, the PWA shows a pairing screen
asking for an 8-character code that Settings displays (valid 10 minutes,
single use, 5 wrong attempts per code invalidate it). Pairing happens **inside
the installed PWA** because iOS gives home-screen apps storage separate from
Safari, so a token obtained in Safari would not carry over.
- On success the server stores the device `{id, name, token_sha256,
  created, last_seen, read_only}`; the PWA keeps the token in `localStorage`.
- Settings lists devices (name, last seen) with **Revoke** (closes its
  connections now) and a **Read-only** toggle (can watch, can't type).

**Platforms (user decision): iPhone/iPad and Android phones/tablets.**
- Apple devices use `https://<LocalHostName>.local:7443`.
- Android uses `https://<LAN IP>:7443`, because `.local` resolution is
  unreliable there. The setup page picks the address per platform. The Remote
  section recommends reserving the Mac's IP on the router (DHCP reservation)
  and shows the current IP.
- The IP is part of the installed app's origin, so if the Mac's IP changes,
  the Android app keeps its old address and stops connecting. The Remote
  section then shows "IP changed — re-add the app on Android devices". The
  server certificate already covers the new IP (§4 re-issue).
- The Origin check accepts exactly the hosts in the current server
  certificate's SANs, with the port.
- Manual end-to-end tests on iPhone, iPad, an Android phone and an Android
  tablet (Chrome).

### 5. Live terminal

- **The marked-session boundary is hard.** A plain `tmux attach` in a pty is
  *not* acceptable: the attached client keeps the tmux prefix key, so the
  phone could press prefix+s / prefix+( / prefix+: to switch to unmarked
  sessions or run arbitrary tmux commands (even a `-r` client can switch).
- Instead the server bridges through **tmux control mode**, and the phone
  only ever sends *bytes for one pane*, never tmux commands:
  - On open: `capture-pane -p -e -t <pane>` (plus cursor position) gives the
    current screen, sent to xterm.js as the initial frame.
  - Live output: a `tmux -C attach -t <session> -f ignore-size,read-only`
    control client owned by the server; it forwards only `%output` lines for
    the marked pane's id (unescaped) and ignores all other notifications.
  - Input: keystroke bytes from the phone are written with
    `send-keys -H -t <pane> <hex…>`, issued by the server. Read-only devices'
    input is dropped server-side. Key bindings never run, because nothing is
    attached as an interactive client.
  - Resize: the phone's size never changes the pane by default (the
    `ignore-size` flag above).
- **Desktop size is protected by default.** The phone renders the
  desktop-sized pane with xterm.js scaled to fit the width, with pinch-zoom and
  a font-size control. A per-session **"Fit to phone"** switch issues
  `resize-window` to the phone's size for when the user is away from the desk.
  The window goes back to its previous size when the switch is turned off or the
  socket closes. *(My call.)*
- The control client and the socket are torn down when the socket closes, the
  tab is unmarked, the device is revoked, or remote is disabled. One control
  client per session can feed many phones.
- Exact control-mode details (escape decoding, pane-id lookup, keeping
  xterm.js in sync after reconnect) are left to the builder. A live test must
  prove that no key sequence from the phone reaches another session.
- Mobile key bar above the keyboard: Esc, Tab, Ctrl-C, Ctrl-D, ↑ ↓ ← →,
  Enter, and a sticky Ctrl modifier.

### 6. Persistence — `~/.openorchestra/remote.json` (0600, atomic)

```json
{
  "enabled": true,
  "port": 7443,
  "keep_awake": true,
  "marked": [{ "worktree_key": "main:comic-chat", "tab_id": 177, "id": "r_x7…" }],
  "devices": [{ "id": "d_…", "name": "Omar's iPhone", "token_sha256": "…",
                "created": "…", "last_seen": "…", "read_only": false }]
}
```
**The file is the single source of truth**, because Dev and release builds
can run at the same time and share it. *(My call, from the review.)*
- Every change (mark, unmark, pair, revoke, read-only, settings) is a locked
  read-modify-write of the file (`flock` on `remote.lock`) applied to the
  file's *current* contents. Never write back an in-memory copy, so no
  `merge_from`-style resurrection of revoked devices or removed marks.
- Pending pairing codes live in the file too (`{code_sha256, expires,
  attempts}`), so a code shown by either app works on whichever one is
  serving.
- The serving instance watches the file and reloads on change. A revoke or
  unmark made in the other build closes connections within a second.
- Only one instance can hold the port. The other one's Remote section says
  "Served by the other openOrchestra build" instead of showing an error.

CA/cert files live in `~/.openorchestra/remote/`. Both builds use the same
CA, so a phone trusts either one.

### 7. Keep-awake

When Remote is enabled and keep-awake is on, the app holds a "prevent idle
system sleep" assertion (`caffeinate -i -w <app pid>` child or an
`IOPMAssertion` — builder's choice), released when either is turned off. The
Remote section notes that a closed MacBook lid still sleeps unless on power
with an external display.

### 8. PWA (Vite + React)

- Lives in `web/remote/` (pnpm). Build output is embedded into the app
  binary at compile time and served from `/`. The Makefile builds it before
  the app; a missing build fails with a clear message rather than silently
  serving nothing.
- Web app manifest (name "openOrchestra Remote", standalone display, theme
  colors, icons from the app's assets incl. Apple touch icon) and a service
  worker that caches the app shell only — never `/api`.
- Screens: **Pair** (code entry) → **Agents** (cards: title, project /
  worktree, agent kind, status pill, time in status, current tool; waiting
  first; tap to open) → **Agent** (xterm.js terminal, key bar, quick-reply
  box, Fit-to-phone switch, back). Connection-lost banner with auto-reconnect;
  a 401 sends the user back to Pair.
- Light/dark follows the system.
- **UI library: shadcn/ui** (Tailwind CSS + Radix primitives), chosen by me
  from the user's shortlist (shadcn, MUI, Blueprint): components are copied
  into the project so the bundle stays small for a phone PWA, and its Sheet /
  Drawer / Dialog / Toast primitives are touch-friendly. MUI is heavier than
  this app needs; Blueprint targets dense desktop UIs and is weak on touch.
  xterm.js stays the terminal; shadcn handles everything around it.
- **Responsive (requested): phone and tablet, portrait and landscape.**
  - Phone (< 768px): one screen at a time — Agents list → Agent view with a
    back button; quick-reply and key bar pinned above the on-screen keyboard
    (use `visualViewport` so the keyboard never covers the input).
  - Tablet (≥ 768px): two panes — the agents list as a left sidebar
    (collapsible) and the selected agent's terminal on the right, so status
    of the others stays visible while typing. Key bar still shown (tablets
    often have no hardware keyboard); hidden when a hardware keyboard is
    detected via a toggle.
  - Touch targets ≥ 44px, safe-area insets respected (notch / home
    indicator), terminal re-fits on rotation and split-view resizing.

### 9. Security summary

Private-IP source filter · TLS from the app's own CA · device tokens (hashed
at rest, revocable, read-only option) · one-time short-lived pairing codes
with attempt limits · opaque ids mapped only through current marks · Origin
check on WebSockets · CSP on the PWA · plain-HTTP port serves only the
public CA and setup page, only while "Add a phone" is open.

### 10. Testing expectations

- Unit: id → session only through marks (unmarked/unknown → 404); token
  auth/revoke/read-only; pairing code expiry and attempt limit; private-IP
  filter; certificate SANs and validity ≤ 397 days; newline flattening;
  remote.json merge.
- Live (`--ignored`, private tmux socket — existing pattern in `tmux.rs`):
  attach/stream/type round-trip; unmark closes an open socket;
  `ignore-size` leaves the window size unchanged; **tmux prefix sequences
  (prefix+s, prefix+(, prefix+:) sent from the phone are delivered to the
  pane as plain bytes and never switch sessions**; revoke or unmark in a
  second process closes the socket.
- Manual: install on an iPhone and an Android phone end to end.

## Left to the builder

HTTP/WebSocket stack (hyper/axum vs. hand-rolled on tokio-rustls +
tungstenite), pty library, cert library, embedding mechanism, PWA tooling
(e.g. `vite-plugin-pwa`), visual styling within shadcn/ui, exact Settings layout, mDNS name
discovery details, and module layout inside `harness-app`.
