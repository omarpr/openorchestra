#!/usr/bin/env bash
# Sign a macOS artifact with Developer ID, notarize it with Apple, and staple
# the resulting ticket.
#
#   ./script/notarize-mac.sh dist/openOrchestra.app          # sign + notarize + staple
#   ./script/notarize-mac.sh dist/openOrchestra-*.dmg        # same, for an image
#   ./script/notarize-mac.sh --sign-only dist/openOrchestra.app
#   ./script/notarize-mac.sh --check                        # credentials only, no artifact
#
# Split out of package-mac.sh deliberately. Notarization is a 2-15 minute
# network round trip to Apple that fails for fiddly reasons (missing timestamp,
# an unsigned nested binary, a malformed entitlement key), and inlining it would
# put every retry behind a ~40-minute universal build. Here the retry loop runs
# against an artifact that already exists.
#
# --sign-only is the entitlements loop: it applies the hardened runtime without
# talking to Apple, so "does this still launch under --options runtime" is a
# seconds-long question rather than a minutes-long one.
#
# Design: docs/superpowers/specs/2026-08-04-signing-notarization-design.md
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# The release team. Asserted against the certificate's OU rather than trusted
# from its display name: on Apple Development certificates the parenthetical in
# the common name is NOT the team ID (see the spec), and this machine's keychain
# also holds certificates from two unrelated teams. Overridable so a fork is not
# hardcoded to ours.
OO_TEAM_ID="${OO_TEAM_ID:-HLWNR3FFHW}"

# Name of the `notarytool store-credentials` profile. Not a secret — a lookup
# key into a keychain that is itself protected — but overridable so a second
# machine or a fork is not forced into this repo's naming.
OO_NOTARY_PROFILE="${OO_NOTARY_PROFILE:-openorchestra}"

# Escape hatch for an ambiguous keychain: a full identity string or a SHA-1.
OO_SIGN_IDENTITY="${OO_SIGN_IDENTITY:-}"

ENTITLEMENTS="${OO_ENTITLEMENTS:-$ROOT/crates/harness-app/resources/openOrchestra.entitlements}"

say()  { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33mwarn:\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

usage() { sed -n '2,10p' "$0" | sed 's/^# \{0,1\}//'; }

sign_only=0
check_only=0
target=""

while (($#)); do
  case "$1" in
    --sign-only) sign_only=1 ;;
    --check)     check_only=1 ;;
    -h|--help)   usage; exit 0 ;;
    -*)          die "unknown flag: $1 (try --help)" ;;
    *)           [ -z "$target" ] || die "expected one artifact, got a second: $1"
                 target="$1" ;;
  esac
  shift
done

# --check is the preflight: it answers "could this machine notarize right now"
# without needing an artifact, so package-mac.sh can ask before it starts a
# ~40-minute build rather than after.
if ((!check_only)); then
  [ -n "$target" ] || { usage; exit 1; }
  [ -e "$target" ] || die "no such artifact: $target"

  case "$target" in
    *.app) kind=app ;;
    *.dmg) kind=dmg ;;
    *)     die "expected a .app or .dmg, got: $target" ;;
  esac
else
  kind=none
fi

# ---------------------------------------------------------------------------
# Identity resolution.
#
# Two independent facts have to line up: the identity must be valid for code
# signing (has a private key, unexpired, unrevoked — that is what find-identity
# reports), and its certificate must belong to OO_TEAM_ID (that is the OU, which
# only the certificate itself carries). Cross-referencing both is what stops a
# perfectly valid certificate from the wrong team signing a release.
# ---------------------------------------------------------------------------

# Emits "<SHA-1>\t<OU>" for every Developer ID Application certificate found.
devid_certs() {
  local line hash="" pem="" ou
  while IFS= read -r line; do
    case "$line" in
      "SHA-1 hash: "*)
        hash="${line#SHA-1 hash: }"; pem="" ;;
      "-----BEGIN CERTIFICATE-----")
        pem="$line"$'\n' ;;
      "-----END CERTIFICATE-----")
        pem+="$line"$'\n'
        ou="$(printf '%s' "$pem" | openssl x509 -noout -subject 2>/dev/null \
              | sed -n 's/.*OU *= *\([^,]*\).*/\1/p')"
        [ -n "$hash" ] && printf '%s\t%s\n' "$hash" "$ou"
        pem="" ;;
      *)
        [ -n "$pem" ] && pem+="$line"$'\n' ;;
    esac
  done < <(security find-certificate -a -Z -p -c "Developer ID Application" 2>/dev/null)
}

resolve_identity() {
  if [ -n "$OO_SIGN_IDENTITY" ]; then
    printf '%s' "$OO_SIGN_IDENTITY"
    return
  fi

  local valid_hashes ou_map matches=() hash ou
  # Hashes that are actually usable for signing right now.
  valid_hashes="$(security find-identity -v -p codesigning 2>/dev/null \
                  | grep 'Developer ID Application' \
                  | sed -E 's/^[[:space:]]*[0-9]+\)[[:space:]]*([0-9A-F]{40}).*/\1/' || true)"

  if [ -z "$valid_hashes" ]; then
    die "no valid 'Developer ID Application' identity in the keychain.
    Create one at https://developer.apple.com/account/resources/certificates
    with the team switcher set to team $OO_TEAM_ID — a certificate created
    under a different team is valid and will publish the wrong entity's name."
  fi

  ou_map="$(devid_certs)"

  while IFS= read -r hash; do
    [ -n "$hash" ] || continue
    ou="$(printf '%s\n' "$ou_map" | awk -F'\t' -v h="$hash" '$1 == h {print $2; exit}')"
    [ "$ou" = "$OO_TEAM_ID" ] && matches+=("$hash")
  done <<< "$valid_hashes"

  if ((${#matches[@]} == 0)); then
    die "found Developer ID identities, but none belonging to team $OO_TEAM_ID.
    Signing with another team would publish the wrong entity in every binary.
    Set OO_SIGN_IDENTITY to override, or OO_TEAM_ID if the team really changed."
  fi

  if ((${#matches[@]} > 1)); then
    # Never guess. Print hashes only — the common names carry personal or
    # organisational identities that do not belong in a terminal scrollback.
    printf 'error: %s Developer ID identities match team %s:\n' \
      "${#matches[@]}" "$OO_TEAM_ID" >&2
    printf '    %s\n' "${matches[@]}" >&2
    die "set OO_SIGN_IDENTITY to one of the hashes above."
  fi

  printf '%s' "${matches[0]}"
}

IDENTITY="$(resolve_identity)"
say "signing identity ${IDENTITY:0:8}… (team $OO_TEAM_ID)"

# notarytool only ever sees a keychain profile name — no secret is passed as an
# argument, since process arguments are readable by any local process for the
# lifetime of the call. `history` is the cheapest call that genuinely round
# trips to Apple, so it proves the credentials work rather than merely exist.
#
# Apple's own error is surfaced, not swallowed: a missing profile and a valid
# profile Apple is refusing (HTTP 403 "A required agreement is missing or has
# expired" — an unsigned Program License Agreement update) used to print the
# same "missing or cannot authenticate", which sends you off recreating
# credentials that were fine all along.
check_notary_profile() {
  local out
  if out="$(xcrun notarytool history --keychain-profile "$OO_NOTARY_PROFILE" 2>&1)"; then
    return 0
  fi
  if printf '%s' "$out" | grep -qi 'agreement'; then
    die "Apple rejected notarization: a developer agreement needs accepting.
    The '$OO_NOTARY_PROFILE' credentials are fine. Sign in to
    https://developer.apple.com/account as the team's Account Holder, accept
    the pending agreement, then retry.
    notarytool said: $(printf '%s' "$out" | tail -1)"
  fi
  die "notarytool profile '$OO_NOTARY_PROFILE' is missing or cannot authenticate.
    notarytool said: $(printf '%s' "$out" | tail -1)
    Create it with:
      xcrun notarytool store-credentials $OO_NOTARY_PROFILE \\
        --key <AuthKey.p8> --key-id <KEYID> --issuer <ISSUERID>"
}

if ((check_only)); then
  say "checking notarytool profile '$OO_NOTARY_PROFILE'"
  check_notary_profile
  say "ready to notarize (identity ${IDENTITY:0:8}…, team $OO_TEAM_ID)"
  exit 0
fi

# ---------------------------------------------------------------------------
# Sign.
#
# No --deep: Apple deprecated it and it pushes our entitlements onto nested
# code. Sign the helpers (voice, Device input) before the outer app bundle.
# --timestamp is required for notarization; without it Apple rejects the
# submission after the upload rather than before it.
# ---------------------------------------------------------------------------
if [ "$kind" = app ]; then
  [ -f "$ENTITLEMENTS" ] || die "entitlements file not found: $ENTITLEMENTS"
  voice_helper="$target/Contents/Helpers/oo-voice"
  if [ -f "$voice_helper" ]; then
    codesign --force --options runtime --timestamp --sign "$IDENTITY" "$voice_helper"
  fi
  device_input_helper="$target/Contents/Helpers/oo-device-input"
  if [ -f "$device_input_helper" ]; then
    codesign --force --options runtime --timestamp --sign "$IDENTITY" "$device_input_helper"
  fi
  say "signing $(basename "$target") (hardened runtime)"
  codesign --force --options runtime --timestamp \
           --entitlements "$ENTITLEMENTS" \
           --sign "$IDENTITY" "$target"
else
  # A disk image is not code: it takes neither the hardened runtime nor
  # entitlements. It is signed so the image itself is tamper-evident.
  say "signing $(basename "$target")"
  codesign --force --timestamp --sign "$IDENTITY" "$target"
fi

say "verifying signature"
codesign --verify --strict --verbose=2 "$target"

if ((sign_only)); then
  say "signed (notarization skipped)"
  exit 0
fi

# ---------------------------------------------------------------------------
# Notarize.
# ---------------------------------------------------------------------------
check_notary_profile

json_get() { python3 -c 'import json,sys; print(json.load(sys.stdin).get(sys.argv[1],""))' "$1"; }

SCRATCH=""
# The `return 0` is load-bearing. SCRATCH is only set on the .app path, so on a
# .dmg the test fails, && short-circuits, and 1 becomes the function's return
# value — which an EXIT trap hands back as the exit status of the whole script.
# A fully successful DMG notarization therefore exited 1, and package-mac.sh's
# set -e killed the release immediately after, having already paid for both
# round trips to Apple. Success reported as failure, at the last possible step.
cleanup() { [ -n "$SCRATCH" ] && rm -rf "$SCRATCH"; return 0; }
trap cleanup EXIT

submit() {
  local artifact="$1" payload="$1" resp id status

  # notarytool takes a .zip, .dmg, or .pkg — never a bare .app — so a bundle is
  # ditto'd first. --keepParent preserves the .app wrapper inside the archive.
  if [ "$kind" = app ]; then
    SCRATCH="$(mktemp -d)"
    payload="$SCRATCH/$(basename "$artifact").zip"
    say "archiving for submission"
    ditto -c -k --keepParent "$artifact" "$payload"
  fi

  say "submitting to Apple (2-15 min; this is the slow part)"
  resp="$(xcrun notarytool submit "$payload" \
            --keychain-profile "$OO_NOTARY_PROFILE" \
            --wait --output-format json 2>/dev/null)" || true

  [ -n "$resp" ] || die "notarytool produced no response — network or credential failure"

  id="$(printf '%s' "$resp" | json_get id)"
  status="$(printf '%s' "$resp" | json_get status)"

  if [ "$status" != "Accepted" ]; then
    # `submit --wait` reports only the status; the actual reason lives in a
    # separate log. Fetching it here is the difference between an actionable
    # failure and the word "Invalid".
    printf '\033[1;31merror:\033[0m notarization %s (submission %s)\n' \
      "${status:-unknown}" "${id:-unknown}" >&2
    if [ -n "$id" ]; then
      printf '\n--- notarytool log ---\n' >&2
      xcrun notarytool log "$id" --keychain-profile "$OO_NOTARY_PROFILE" >&2 2>&1 || true
      printf -- '--- end log ---\n\n' >&2
    fi
    die "not stapling or publishing a rejected build."
  fi

  say "accepted (submission $id)"
}

submit "$target"

# ---------------------------------------------------------------------------
# Staple, so first launch works without a network round trip to Apple. Both the
# .app and the .dmg get their own ticket: notarizing only the image would leave
# an app dragged to /Applications with no ticket of its own, and it would fail
# to launch offline or behind a firewall blocking Apple's OCSP responder — the
# exact failure this whole path exists to eliminate.
# ---------------------------------------------------------------------------
say "stapling ticket"
xcrun stapler staple "$target"
xcrun stapler validate "$target"

# ---------------------------------------------------------------------------
# Verify the way Gatekeeper will. codesign proves the bytes are well-formed;
# only spctl answers "would this open on someone else's Mac".
# ---------------------------------------------------------------------------
say "Gatekeeper assessment"
if [ "$kind" = app ]; then
  spctl -a -vvv -t exec "$target"
else
  spctl -a -vvv -t open --context context:primary-signature "$target"
fi

say "notarized and stapled: $target"
