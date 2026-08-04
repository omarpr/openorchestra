# Developer ID signing and notarization

**Date:** 2026-08-04
**Status:** draft

## Problem

`script/package-mac.sh:162-165` ad-hoc signs the bundle with `codesign --force
--deep --sign -`. An ad-hoc signature carries no identity, so Gatekeeper
quarantines every download and macOS reports the app as *damaged* on a plain
double-click. The release notes ship an apology for it — `INSTALL_NOTE`
(`package-mac.sh:198-202`) instructs every user to right-click → Open and
explains that the build "is ad-hoc signed, not notarized."

The 2026-07-26 packaging spec put this out of scope, "blocked on an Apple
Developer account; the hook is left in place." That block is now lifted: the
`Soto Torres Group LLC` organization enrollment (Team ID `HLWNR3FFHW`) exists.

Goal: a downloaded DMG opens on a double-click, on a machine that has never seen
the app, without instructions.

## Facts established before designing

Recorded because several of them contradict the obvious assumption.

**No Developer ID certificate exists yet.** `security find-identity -v -p
codesigning` returns five identities, all of them `Apple Development` — a type
that signs for local development and TestFlight only. Notarization rejects
submissions signed with it. Creating the certificate is a prerequisite, not a
step in the script.

**The Team ID is not the parenthetical in the identity name.** Parsing the
certificates rather than the display strings:

| CN (what `find-identity` prints)              | OU (real Team ID) | O                              |
|-----------------------------------------------|-------------------|--------------------------------|
| `Apple Development: <redacted> (86XK7HVEUY)`   | `3636ZAJ7K2`      | an individual enrollment       |
| `Apple Development: <redacted> (59R7T97Q6V)`   | `5JVWSC4J2R`      | a second, unrelated individual |

(Identities redacted deliberately — reproduce locally with
`security find-certificate -a -p | openssl x509 -noout -subject`.)

`86XK7HVEUY` looks like a Team ID and is not one. Any code deriving the team
from the common name would silently produce the wrong value.

**Three teams are in play, two of them wrong.** The keychain holds certificates
from `3636ZAJ7K2` (an individual enrollment under a personal legal name) and
`5JVWSC4J2R` (a different person entirely). The release team, `HLWNR3FFHW`, has
no certificate on this machine yet. A certificate created from the wrong team is
a perfectly valid certificate that publishes the wrong entity's name in every
shipped binary, so team selection needs a machine-checked guard rather than
care at creation time.

**Signing identity is public; the notarization key is not.** The certificate's
common name and Team ID are embedded in every signed binary and readable with
`codesign -dvvv`. Because the enrollment is an organization, that string is
`Soto Torres Group LLC (HLWNR3FFHW)` and no personal name is disclosed. The App
Store Connect `.p8` key, its Key ID, and Issuer ID are secret and must never
reach the repository, the environment, or a process argument.

**Tooling is already present.** `notarytool` 1.1.2, `stapler`, and Xcode 26.6.
Nothing to install; `setup-macos.sh` is untouched.

**Hardened runtime is required for notarization, and blocks JIT by default.**
`wasmtime 36.0.13` is in the graph via `tree-sitter 0.26.11` (not via zed's
extension host, as assumed at first). Whether that JIT executes at runtime is
unknown — zed bundles native grammars, so it may never be reached. This is
resolved by measurement, not assumption; see *Entitlements*.

**App Sandbox is not required and is not adopted.** Notarization requires only
the hardened runtime. Sandboxing would break the premise of an app that spawns
agents, shells, and git across arbitrary worktrees.

## Prerequisites (one-time, manual)

1. **Developer ID Application certificate**, created at developer.apple.com →
   Certificates, **with the team switcher set to `Soto Torres Group LLC`**.
   Creating it under `3636ZAJ7K2` is the most likely mistake and produces a
   working certificate that leaks a personal legal name. Developer ID creation
   may be restricted to the Account Holder; if the type is absent from the list,
   that is the cause.
2. **App Store Connect API key** — Users and Access → Integrations → Keys, role
   **Developer** (sufficient; Admin is not needed). The `.p8` downloads once and
   is unrecoverable afterwards. Store outside the repository, `chmod 600`.
3. **Keychain profile:**
   ```sh
   xcrun notarytool store-credentials openorchestra \
     --key ~/private_keys/AuthKey_<KEYID>.p8 --key-id <KEYID> --issuer <ISSUERID>
   ```
   After this the scripts reference only the profile name.

## Design

### Where the logic lives

A new `script/notarize-mac.sh` takes a path to a `.app` or `.dmg` and performs
identity resolution, signing, submission, stapling, and verification.
`package-mac.sh` calls it twice and stays roughly its current length.

The reason for extraction is iteration cost. Notarization is the one step that
cannot be exercised in a tight loop — it is a network round trip to Apple taking
2-15 minutes that fails for fiddly reasons (missing timestamp, unsigned nested
binary, malformed entitlement key). Inlined, every attempt at a fix re-enters a
script whose first act is a ~40-minute universal build. Extracted, the loop runs
against an artifact that already exists:

```sh
./script/notarize-mac.sh dist/openOrchestra.app
```

It also matches the existing shape, where `package-mac.sh`, `setup-macos.sh`,
and `doctor.sh` each do one job.

### Entry points

`--notarize` is an explicit flag; `--publish` implies it. `make app` and
`make dmg` remain ad-hoc signed, fast, and usable by a contributor with no Apple
account. A new `make dmg-notarized` runs the full path without publishing, so
the first notarized DMG ever produced is not the one that goes public. Because
`--publish` implies `--notarize`, a release cannot ship unnotarized by accident.

### Identity resolution

Candidates come from `security find-identity -v -p codesigning`, filtered to
`Developer ID Application`. Then:

- **Zero matches** — fail with the portal URL and the team-switcher warning.
- **More than one** — fail, listing each candidate with its SHA-1, and point at
  `OO_SIGN_IDENTITY` to disambiguate. Never guess.
- **OU assertion** — the resolved certificate's OU must equal `HLWNR3FFHW`.
  This is what makes a stray certificate from `3636ZAJ7K2` or `5JVWSC4J2R`
  unable to sign a release, and it is checked against the certificate, not the
  display string.

Signing always references a full identity string or SHA-1, never a prefix.

### Pipeline order

```
1. assemble bundle              (existing, unchanged)
2. strip -x                     (existing — must stay BEFORE signing)
3. codesign the .app            --force --options runtime --timestamp
                                --entitlements crates/harness-app/resources/
                                               openOrchestra.entitlements
4. codesign --verify --strict   fail before spending a round trip
5. ditto -c -k --keepParent → .zip ;  notarytool submit --wait
6. stapler staple the .app
7. hdiutil create the DMG       (existing) — now from a stapled app
8. codesign the .dmg            --timestamp; no runtime or entitlements
9. notarytool submit --wait ;  stapler staple the .dmg
10. verification (below)
```

`--deep` does not survive. Apple deprecated it, and it pushes entitlements onto
nested code. The bundle contains exactly one Mach-O, so signing the bundle
directly is correct and sufficient.

**Two round trips, not one.** Notarizing only the DMG would let a downloaded DMG
open, since the ticket covers the enclosed app's cdhash and Gatekeeper checks
online. But the `.app` dragged to `/Applications` would carry no stapled ticket
of its own, so a first launch while offline — or behind a firewall blocking
Apple's OCSP responder — fails with the same "damaged" dialog this work exists
to eliminate. The cost is one extra wait, on release day only.

`touch "$APP"` (`package-mac.sh:168`) is retained and is safe: signatures hash
contents, not directory mtimes. Step 4 now runs after it, so if that ever stops
being true it surfaces at build time rather than at a user's first launch.

### Entitlements

`crates/harness-app/resources/openOrchestra.entitlements`, matching zed's
`crates/zed/resources/zed.entitlements` convention.

**The file starts as an empty `<dict/>`** — valid to pass to `codesign`, and it
keeps the flag in the pipeline unchanged as entries appear. Entries are added
only where a demonstrated failure requires them. Zed's file is not a template: most of its entries
(camera, audio input, address book, calendars, location, photos) exist for
collab and screen-sharing features that this app deliberately keeps inert via
`BlockedHttpClient` (`workspace_host.rs:72`), and its
`files.user-selected.read-write` entries are App Sandbox keys, inert unsandboxed.
Copying it would ship unjustified privacy entitlements and leave nobody able to
explain any line.

The candidates, should they prove necessary, are `com.apple.security.cs.allow-jit`
and `com.apple.security.cs.allow-unsigned-executable-memory`, both for the
tree-sitter/wasmtime path. Each entitlement added carries a comment recording
the failure that justified it.

### Credentials

`notarytool` is invoked only as `--keychain-profile "$OO_NOTARY_PROFILE"`,
defaulting to `openorchestra`. The variable exists so a fork or a second machine
is not forced into this repository's naming — the profile name is not a secret,
being merely a lookup key into a keychain that is itself protected.

No secret is passed as a command-line argument, since process arguments are
readable by any local process for the lifetime of the call.

### Preflight

Identity presence, OU match, and keychain-profile validity are checked next to
the existing `gh auth` checks at `package-mac.sh:61-78`, before any compilation.
This follows the rule already stated there: check everything that can fail
*before* starting a 40-minute build. A missing certificate fails in seconds.

### Failure handling

`notarytool submit --wait` reports only `status: Invalid` — the actual reason
lives in a separate log. On any non-`Accepted` status the script runs
`notarytool log <submission-id> --keychain-profile ...`, prints it, and exits
non-zero. Without this the operator gets "Invalid" and no way forward.

Nothing is stapled and nothing is published on failure. A half-finished release
is worse than no release.

### Documentation changes

- `INSTALL_NOTE` (`package-mac.sh:198`) becomes conditional. Notarized builds
  say to drag to Applications and double-click; the right-click → Open text
  survives only for ad-hoc builds.
- `README.md` drops the implication of an alarming first launch.
- `doctor.sh` gains a "Release signing" section reporting certificate and
  profile presence. Absence means "cannot cut releases", not "app is broken".
- The 2026-07-26 spec's "Out of scope" entry gains a pointer to this document
  rather than being edited, so the history stays readable.

## Verification

- `codesign --verify --strict --verbose=2` on the bundle.
- `codesign -dvvv` reports `Authority=Developer ID Application: Soto Torres
  Group LLC (HLWNR3FFHW)` and `flags=0x10000(runtime)`.
- `stapler validate` on both the `.app` and the `.dmg`.
- `spctl -a -vvv -t exec` on the app reports `source=Notarized Developer ID`.
  This is the check that answers the user's actual question; the others confirm
  only that the bytes are well-formed.
- **Quarantine simulation** — the failure mode under test only appears on a
  quarantined artifact. Download the published DMG through a browser on a
  machine that has never run the app, and double-click it. Nothing short of this
  exercises what a user experiences.
- The entitlements pass: sign with `--options runtime` locally (no notarization,
  so the loop is seconds) and exercise project open and syntax highlighting (the
  tree-sitter path), terminals, a live agent, the diff panel, markdown preview
  with a mermaid diagram, and notifications.
- `shellcheck script/notarize-mac.sh script/package-mac.sh`.
- `make app` still produces a working ad-hoc bundle — no regression for
  contributors without credentials.

## Out of scope

- **CI-based releases.** Releases continue to be cut from the author's machine;
  revisit when that stops being true. The credential design does not preclude
  it — an App Store Connect key is the form that ports to CI unchanged.
- **App Sandbox.** Not required for notarization, and incompatible with running
  agents in arbitrary worktrees.
- **Sparkle or any auto-update mechanism.** Signing is a prerequisite for it,
  not a part of it.
- **Windows and Linux packaging.**
