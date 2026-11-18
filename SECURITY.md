# Security policy

## Reporting a vulnerability

Please **don't** open a public issue. Report it privately through GitHub:
**Security → Report a vulnerability** on this repository
([direct link](https://github.com/omarpr/openorchestra/security/advisories/new)).

Include what you found, how to reproduce it, and what an attacker could do with
it. You'll get an acknowledgement within a few days. Fixes ship in the next
release, and you'll be credited in the advisory unless you'd rather not be.

## Supported versions

Only the latest release gets security fixes. Update with
`brew upgrade --cask openorchestra` or by downloading the newest `.dmg`.

## Scope

openOrchestra runs entirely on your Mac. The parts most worth a look:

- **Phone remote control** (`crates/remote`): the HTTPS server, pairing codes,
  device tokens, the local certificate authority, and the LAN-only peer filter.
- **The A2A broker** (`crates/a2a-broker`): the localhost agent-to-agent
  channel.
- **Setup and teardown scripts**: committed `.openorchestra/hooks.toml` scripts
  from a cloned repository run only after you confirm.
- **State under `~/.openorchestra`**: written with private permissions.

Out of scope: vulnerabilities in the agent CLIs themselves (Claude Code, Codex
and others), in tmux, or in zed's crates. Please report those upstream.

Dependency advisories are checked on every `Cargo.toml`/`Cargo.lock` change and
weekly by [`security-audit.yml`](.github/workflows/security-audit.yml).
