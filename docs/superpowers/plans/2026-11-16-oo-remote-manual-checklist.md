# oo-remote manual checklist

Things automated tests could not check. Use a scratch build and a scratch HOME
(never your running app). Have a phone in hand. Each section says where to look.

Run sections C–E and G once per device: iPhone, iPad, Android phone,
Android tablet (Chrome). Note the device at the top of each pass.

Device: ______________  Date: __________

## A. On the Mac: tabs and sidebar

- [ ] Right-click a terminal tab and an agent tab: a menu shows "Allow remote control". Shells also show "Gives a paired phone this shell".
- [ ] Editor, diff and chat tabs show no menu. The menu sits under the tab strip near the click and stays on screen. Clicking a menu item works (the backdrop doesn't eat it).
- [ ] After marking: a dot and a "Remote control on" tooltip appear before the tab title, and a dot on the sidebar row (main checkout, worktree rows, task members).
- [ ] Marking while Remote is off shows "Marked - turn on Remote control in Settings to reach it".
- [ ] Choosing "Stop remote control" removes the dots. Closing a marked tab removes its mark.
- [ ] A stale tab menu doesn't linger when you click elsewhere.
- [ ] Top bar chip for the current terminal/agent tab: "📱 Remote off" when not allowed; "On phone" with a dot that fades in and out while a phone can reach it; "On phone · remote off" when the tab is allowed but Remote is off in Settings. Editor/diff tabs show no chip.
- [ ] Remote dots on tabs and sidebar rows fade in and out while phones can reach them, and stop fading (steady, dimmed) when Remote is off.

## B. On the Mac: Settings → Remote

- [ ] "Remote" sits between Notifications and Shortcuts in the Settings list.
- [ ] Click On: "Starting…" then "Listening on https://<name>.local:7443/ and https://<ip>:7443/". Click Off: "Off".
- [ ] Under the Port field: "Changing the port means re-adding the app on paired phones."
- [ ] Each listening URL is on its own row with a Copy button; Copy puts that exact URL on the clipboard.
- [ ] "Agents on phone" lists every terminal and agent tab with an On/Off toggle; toggling matches the tab's right-click mark and the dots.
- [ ] Port: type 80 then Enter shows red "Use a port from 1024 to 65535". Type 7080 shows "Port 7080 is used to set up phones". Type 9443: status passes through "Starting…" and lists :9443 URLs; the window doesn't freeze. `lsof -i :7443` is empty and `lsof -i :9443` is listening.
- [ ] Opening Settings with Cmd+, from an editor tab and typing goes into the Port field.
- [ ] Add a phone: the QR is black on white with a white margin in both light and dark themes. A phone camera opens `http://<ip>:7080/setup`.
- [ ] The CA fingerprint lines are readable and match what the phone's setup page shows.
- [ ] The pairing code reads `XXXX-XXXX`. "New code" changes it. An old code still works until it expires (10 minutes).
- [ ] `lsof -i :7080` listens only while the panel is open. It stops after Done, the close button, a backdrop click, Cmd+, switching sections, or turning Remote Off.
- [ ] With the panel open, change the port: the setup page at :7080 still works after the restart.
- [ ] A paired phone shows "last seen ..." time.
- [ ] Watch only toggles; the phone then can't type.
- [ ] Revoke becomes a red "Confirm revoke". Waiting 4 seconds puts it back. A second click removes the device and cuts the phone off.
- [ ] The Android tip is always visible. The stale-IP notice shows after the Mac's IP changes with an Android device paired by IP.
- [ ] Keep awake On: `pgrep caffeinate` finds a process while serving. Off or stopped: it's gone.
- [ ] Quit the app while a phone is connected: the app exits promptly (a couple of seconds), with no hang.
- [ ] Quit openOrchestra while a phone has Fit to phone on: after reopening (or in another tmux client), the desktop window is back to its own size, not phone size.
- [ ] On a Mac with a VPN or VM interface: a full-tunnel VPN's address is expected in the Add a phone QR and the Android link (that is the address the Mac uses to reach the network). With a split-tunnel VPN or a VM bridge (Parallels, VMware), the QR and link use the Wi-Fi/Ethernet address, not the VM/VPN one.

## C. Phone, first time

- [ ] iPhone/iPad: open the setup page in Chrome (or another non-Safari browser): a banner says to open it in Safari and gives an address to copy. In Safari there is no banner.
- [ ] Scan the QR from Add a phone. The setup page opens over http on port 7080.
- [ ] iPhone/iPad: install the profile (Settings → Profile Downloaded), then Settings → General → About → Certificate Trust Settings, and turn on full trust for "openOrchestra Remote CA".
- [ ] Android: install the CA certificate (Settings → Security → Encryption and credentials → Install a certificate → CA certificate) and accept the warning.
- [ ] The fingerprint shown on the phone matches the Mac's Settings.
- [ ] iPhone/iPad: open `https://<name>.local:7443`. No certificate warning.
- [ ] Android: open `https://<Mac IP>:7443`. No certificate warning.
- [ ] Android: `https://<name>.local:7443` may not load. That is expected. Using the IP works.
- [ ] iPhone/iPad in Safari, before installing: the page shows "Add openOrchestra to your Home Screen" first. After installing, open the Home Screen app and pair there. "Pair in this browser instead" pairs in Safari without installing.
- [ ] Add to home screen (Share → Add to Home Screen on iOS; Install app in Chrome on Android). The app opens full screen with the right icon and name, with no browser bar.
- [ ] The Pair screen asks for the code. Type it (the dash is added for you, lowercase becomes uppercase). A wrong code shows an error; after 5 wrong tries codes stop working until you press "New code".
- [ ] A correct code pairs and shows the agent list. The device appears in Settings.

## D. Phone, everyday use

- [ ] Only marked agents appear. Unmarked tabs and shells never show.
- [ ] The header reads "openOrchestra Agents" and each agent shows its icon. Status pills: RUNNING (spinner), WAITING (breathing amber), STOPPED (outline); an idle agent shows no pill.
- [ ] Status dots update live (running, waiting, idle) when an agent changes state on the Mac.
- [ ] Open an agent: the terminal fills in and shows live output as the agent works.
- [ ] Type with the key bar (Esc, Tab, Ctrl, arrows, Enter, etc.) and the text arrives in the agent.
- [ ] Quick reply sends text and presses Enter in the agent.
- [ ] Quick reply into a real agent (for example Claude Code) submits the message, not just types it into the prompt.
- [ ] Fit to phone on: the text re-flows to the phone width and the Mac's pane isn't broken. Fit off: it returns to the original size and redraws cleanly. Also check the font-size stepper and pinch zoom.
- [ ] The on-screen keyboard coming up doesn't hide the key bar or the cursor line.
- [ ] Prefix keys typed from the phone (for example Ctrl+B then a digit or n/p) don't switch sessions on the Mac.
- [ ] Phone (under 768px): list, then full-screen agent with a Back button. Tablet (768px and up): list on the left, terminal on the right, with Hide/Show agent list. The thin "Show agent list" row is acceptable above the header.
- [ ] Rotate the device: layout adapts, terminal redraws, nothing is cut off by the notch or home bar.
- [ ] iPad / Android tablet: use split view or a narrow window; the layout switches between phone and tablet at 768px.
- [ ] Light and dark mode follow the system setting. Touch targets are comfortable.
- [ ] Lock the phone for 2 minutes and unlock: the app reconnects and the terminal redraws with current content.
- [ ] With Fit to phone on, lock the phone or switch to another app: the desktop window returns to its own size. Come back: it fits to the phone again.
- [ ] Turn Wi-Fi off and on: the connection indicator shows offline, then recovers.

## E. Changes on the Mac while a phone is connected

- [ ] With a tmux config that has `set -g detach-on-destroy off`, kill the agent the phone is watching (for example `tmux kill-session`): the phone shows "Agent stopped".
- [ ] Unmark the tab the phone is viewing: a banner appears on the phone and the agent leaves the list.
- [ ] Turn Remote Off: the phone shows "Remote control is off on the Mac". Turn it On: the phone recovers.
- [ ] Set the device to Watch only: typing is refused. Set it back: typing works.
- [ ] Revoke the device: the phone returns to the Pair screen. Pairing again with a new code works.
- [ ] Change the Mac's IP (switch Wi-Fi or reconnect): the stale-IP notice appears in Settings for an Android device paired by IP; open the new address on the phone.
- [ ] Change the port: every phone's saved address and installed app points at the old port. Re-add the app on each phone with the new address and re-pair as needed.

## F. Dev and release builds together

- [ ] Run a Dev build and a release build at the same time, both with Remote On. One serves; the other shows "Served by the other openOrchestra build" in amber, with a line saying tabs marked there reach phones through the other build.
- [ ] In the non-serving build, mark a tab: the note says phones reach it through the other openOrchestra build, and the agent appears on the phone within a few seconds.
- [ ] Revoke a device from either build: the phone is cut off within about 1 second.
- [ ] Quit the serving build: the other can start serving when you turn Remote Off and On.

## G. Conversation view (Claude Code and pi)

Use a Claude Code agent and a pi agent marked for remote control.

- [ ] Open a Claude Code or pi agent that has a transcript: the header reads `[icon] repo · branch · agent`, with a Conversation | Terminal switch and Conversation selected. A brand-new agent shows Terminal only until its first message. Leave on Terminal, close and reopen the agent: the last choice is remembered.
- [ ] Your earlier messages and the agent's replies appear verbatim, with Markdown (lists, code blocks, links) rendered. Images in replies are not shown.
- [ ] A tool call is one line with a check mark on success or a cross on failure. Tap it: a sheet shows the output. Output over 64 KB ends with "…[truncated]".
- [ ] Thinking is collapsed. With text: "💭 Thinking…" plus its first line; tap to expand. Without text: "💭 Thought for N s" when a duration was recorded, otherwise "💭 Thinking (not recorded)".
- [ ] Send a message from the reply box: it appears in the conversation and the agent answers.
- [ ] Status bar while the agent works: three bouncing dots, "Working…" and the current tool name. With the device's reduced-motion setting on, the dots do not bounce.
- [ ] When the agent starts waiting for you (permission or question): the status bar turns amber and reads "Needs your input", and the reply box is focused automatically once (Conversation view only; there is nothing to tap).
- [ ] Quit the agent on the Mac: the bar reads "Agent stopped" and the reply box goes away.
- [ ] Scroll up while the agent keeps writing: a "New messages ↓" pill appears and the view does not jump. Tap it: you land at the bottom and the pill goes away. At the bottom the view follows new messages.
- [ ] In Claude Code, rewind to an earlier message (Esc Esc): the conversation redraws to match, without duplicates.
- [ ] Start a new session in the same tab (/clear or /new): the phone switches to the new conversation.
- [ ] Switch to Terminal and back: the terminal works as in section D and the conversation keeps its place.
- [ ] A Codex agent shows the Terminal only, with no switch.
- [ ] Open an old link to an agent that was removed or unmarked: "Agent not found".
- [ ] Reconnect after locking the phone: the conversation catches up with no gaps.

## H. Projects sidebar order

- [ ] With 2 or more projects, the ⇅ button to the left of PROJECTS' + opens "Reorder projects" with a four-dot grip on each row (with fewer than 2 projects the button is absent). Dragging a row by its grip reorders the list, and the sidebar order follows after closing the panel.
- [ ] A project's menu has "Move up" and "Move down" (absent at the ends of the list); each moves the project one place.
- [ ] Quit and reopen: the order is kept.
