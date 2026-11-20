# Remote control from a phone or tablet

openOrchestra shares selected agent and shell tabs over HTTPS on your local
network. Keep the phone or tablet on the same network as your Mac.

## Pair a device

1. On the Mac, open **Settings → Remote** and turn remote control on. The
   default HTTPS port is 7443.
2. Click **Add a device** and scan the setup QR code. Follow the instructions
   to install and trust the app’s local certificate authority. Check that the
   certificate fingerprint shown on the device matches the one in Settings.
3. Open the HTTPS address displayed on the Mac. On Android, use the Mac’s IP
   address if its `.local` name fails to resolve. On iPhone or iPad, Safari
   offers to add openOrchestra to the Home Screen so you can pair in the
   installed web app. You can also choose **Pair in this browser instead**.
4. Enter the eight-character pairing code from the Mac. The code works once
   and expires after 10 minutes.

If the Mac’s IP address or remote port changes, update the address on the
device or add the web app to your Home Screen again. Then get a new pairing
code from the Mac and pair again: the device keeps its pairing per address,
so the new address starts unpaired.

## Share and use tabs

Right-click an agent or shell tab on the Mac and select **Allow remote control**.
Use **Settings → Remote → Agents on phone** to manage shared tabs in one place.
The top bar shows whether the selected tab is available remotely.

Open a shared tab on your device to see its status and interact with it.
Conversation view becomes available when a supported agent produces a
transcript. Claude Code, Codex, OpenCode 2.x, and pi are supported. You can
switch to **Terminal** at any time. For writable tabs, you can attach PNG,
JPEG, and WebP screenshots up to 8 MiB. Dictation uses on-device transcription
on supported Macs running macOS 26 or later.

In **Settings → Remote**, set a paired device to **Watch only** to disable input
or select **Revoke** to remove access. You can also keep the Mac awake while
remote control is on.

The remote client lives in [`web/remote`](../web/remote), and its server is in
[`crates/remote`](../crates/remote).
[Manual test checklist](superpowers/plans/2026-11-16-oo-remote-manual-checklist.md)
