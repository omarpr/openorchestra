# openOrchestra

A macOS harness for orchestrating coding agents — pi, Claude Code, Codex,
OpenCode, and Kimi — across git worktrees, with a real embedded zed editor,
per-worktree terminals, diff review, and native notifications when an agent
needs you. Tasks that span several repos get one specialist agent per repo,
coordinated through a localhost [A2A](docs/architecture.md) broker
(`crates/a2a-broker`).

## Run it (macOS)

```sh
make setup && make app && open dist/openOrchestra.app
```

- `make setup` installs/verifies the build and runtime tools (idempotent).
- `make app` builds a release bundle for the host architecture into
  `dist/openOrchestra.app`.
- `make dmg` builds a universal (Apple Silicon + Intel) bundle and a `.dmg`
  beside it. `make dmg-notarized` additionally signs it with Developer ID and
  notarizes it with Apple, so the download opens on a plain double-click;
  `make release` does that and publishes to GitHub Releases.
- First launch will ask for notification permission — that's the
  "agent needs your attention" banners with their View button.

For development iterations, `make run` builds and launches the debug app in a
separate `.app` bundle. This lets the development build use native macOS
notifications; logs remain attached to the terminal.

## Remote control from your phone

You can follow and type into your agents from a phone or tablet on the same
home network. Only tabs you mark are reachable; everything else on the Mac
stays invisible.

1. Right-click a terminal or agent tab and choose **Allow remote control**.
2. Open **Settings → Remote** and turn it **On** (HTTPS on port 7443).
3. Click **Add a phone** and scan the QR code. It opens a short setup page that
   installs and trusts the app's own certificate authority (an iOS profile, or
   a CA certificate on Android). Check that the fingerprint on the phone
   matches the one in Settings.
4. On the phone, open `https://<your-mac-name>.local:7443` (iPhone/iPad), or
   `https://<Mac IP address>:7443` (Android), and add it to the home screen.
5. Enter the 8-character pairing code shown in Settings. It works once and
   expires after 10 minutes.

Android tip: `.local` names often don't resolve on Android, so use the Mac's IP
address. If the Mac's IP changes, Settings shows a notice and the phone's saved
address needs updating. Changing the port also means re-adding the app on each
phone.

In Settings you can set a device to **Watch only** or **Revoke** it, and keep
the Mac awake while serving. Phones on the local network only (private
addresses) are accepted. A manual test list is in
[`docs/superpowers/plans/2026-11-16-oo-remote-manual-checklist.md`](docs/superpowers/plans/2026-11-16-oo-remote-manual-checklist.md).
The PWA source is in `web/remote` (`make web` builds it); the server is the
`crates/oo-remote` crate.

## Contributors

<table>
  <tr>
    <td align="center">
      <a href="https://github.com/omarpr">
        <img src="https://avatars.githubusercontent.com/u/4412916?v=4" width="88" height="88" alt="omarpr"><br>
        <sub><b>omarpr</b></sub>
      </a>
    </td>
    <td align="center">
      <a href="https://github.com/joeswiggin">
        <img src="https://avatars.githubusercontent.com/u/4819705?v=4" width="88" height="88" alt="joeswiggin"><br>
        <sub><b>joeswiggin</b></sub>
      </a>
    </td>
    <td align="center">
      <a href="https://github.com/jakesorce">
        <img src="https://avatars.githubusercontent.com/u/801470?v=4" width="88" height="88" alt="jakesorce"><br>
        <sub><b>jakesorce</b></sub>
      </a>
    </td>
  </tr>
</table>

## Contributing

[`CONTRIBUTING.md`](CONTRIBUTING.md) covers the setup, the project invariants,
and the one build gotcha that bites everyone: `crates/harness-app` is outside the
root workspace, so a root `cargo build` skips it and still reports success.
By taking part you agree to the [Code of Conduct](CODE_OF_CONDUCT.md).

## License

[GPL-3.0-or-later](LICENSE).

Not a preference — a consequence. openOrchestra embeds a real zed editor, and the
crates it links (`workspace`, `project`, `editor`, `git_ui`, `search`,
`terminal`, `theme`, and the rest) are GPL-3.0-or-later, so anything distributed
with them must be too. `gpui` and `util` are Apache-2.0, which combines into a
GPLv3 work in that direction.
