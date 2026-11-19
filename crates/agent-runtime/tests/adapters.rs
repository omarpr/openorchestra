//! A2–A4 tests — the peer adapters (architecture §4.5).
//!
//! The OpenCode tests stand up a real TCP server speaking real HTTP/1.1 + SSE,
//! so the hand-rolled client in `sse.rs` is exercised against actual bytes
//! rather than a mock of itself.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Both codex tests point HARNESS_CODEX_HOME at their own tempdir; the
/// variable is process-global, so they must not run concurrently.
static CODEX_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

use agent_runtime::sse::SseStream;
use agent_runtime::{
    AgentKind, AgentRuntime, AgentSignal, AgentState, ClaudeCodeRuntime, CodexRuntime,
    ConfigurableRuntime, GrokRuntime, HookInstall, KimiRuntime, OpenCodeRuntime, SignalKind,
    SignalProfile, WaitFidelity, WaitReason,
};

const RECV: Duration = Duration::from_secs(10);

// ---- SSE client ------------------------------------------------------------

/// Serve one canned SSE response, optionally chunked, then close.
fn serve_once(body: &'static str, chunked: bool) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        // Drain the request headers.
        {
            let mut reader = BufReader::new(sock.try_clone().unwrap());
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 {
                if line == "\r\n" {
                    break;
                }
                line.clear();
            }
        }
        if chunked {
            sock.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                  Transfer-Encoding: chunked\r\n\r\n",
            )
            .unwrap();
            // Deliberately split mid-frame so the un-chunker has to reassemble.
            let (a, b) = body.split_at(body.len() / 2);
            for part in [a, b] {
                write!(sock, "{:x}\r\n", part.len()).unwrap();
                sock.write_all(part.as_bytes()).unwrap();
                sock.write_all(b"\r\n").unwrap();
            }
            sock.write_all(b"0\r\n\r\n").unwrap();
        } else {
            sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n")
                .unwrap();
            sock.write_all(body.as_bytes()).unwrap();
        }
        sock.flush().unwrap();
    });
    port
}

const EVENTS: &str = "event: session.updated\ndata: {\"type\":\"session.updated\"}\n\n\
event: permission.updated\ndata: {\"type\":\"permission.updated\",\"id\":\"p1\"}\n\n\
event: session.idle\ndata: {\"type\":\"session.idle\"}\n\n";

#[test]
fn sse_client_reads_plain_event_stream() {
    let port = serve_once(EVENTS, false);
    let events: Vec<_> = SseStream::connect("127.0.0.1", port, "/event")
        .unwrap()
        .collect();
    assert_eq!(events.len(), 3, "{events:#?}");
    assert_eq!(events[0].event.as_deref(), Some("session.updated"));
    assert_eq!(events[1].event.as_deref(), Some("permission.updated"));
    assert!(events[1].data.contains("\"id\":\"p1\""));
    assert_eq!(events[2].event.as_deref(), Some("session.idle"));
}

#[test]
fn sse_client_handles_chunked_transfer_encoding() {
    // A streaming endpoint normally responds chunked, and the chunk boundaries
    // do not respect event boundaries — the client must reassemble.
    let port = serve_once(EVENTS, true);
    let events: Vec<_> = SseStream::connect("127.0.0.1", port, "/event")
        .unwrap()
        .collect();
    assert_eq!(events.len(), 3, "{events:#?}");
    assert_eq!(events[1].event.as_deref(), Some("permission.updated"));
}

#[test]
fn sse_client_joins_multiline_data_and_skips_comments() {
    let port = serve_once(
        ": keep-alive\ndata: one\ndata: two\n\nid: 7\ndata: three\n\n",
        false,
    );
    let events: Vec<_> = SseStream::connect("127.0.0.1", port, "/event")
        .unwrap()
        .collect();
    assert_eq!(events.len(), 2, "{events:#?}");
    assert_eq!(events[0].data, "one\ntwo");
    assert_eq!(events[1].data, "three");
}

#[test]
fn sse_client_rejects_a_non_200_response() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        let _ = sock.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
    });
    assert!(SseStream::connect("127.0.0.1", port, "/event").is_err());
}

#[test]
fn sse_connect_fails_cleanly_when_nothing_is_listening() {
    // The agent's server is not up until the user starts it, so this is the
    // normal case, not an error path.
    let port = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    }; // listener dropped -> port closed
    assert!(SseStream::connect("127.0.0.1", port, "/event").is_err());
}

// ---- OpenCode adapter ------------------------------------------------------

#[test]
fn opencode_event_classification_is_conservative() {
    use agent_runtime::sse::SseEvent;
    let classify = |name: &str, data: &str| {
        let _ = SseEvent::default();
        agent_runtime::opencode_classify(&Some(name.to_string()), data)
    };
    assert_eq!(
        classify("permission.updated", "{}"),
        Some((AgentState::Waiting, WaitReason::PermissionPrompt))
    );
    assert_eq!(
        classify("session.idle", "{}"),
        Some((AgentState::Idle, WaitReason::TurnComplete))
    );
    assert_eq!(
        classify("message.part.updated", "{}"),
        Some((AgentState::Running, WaitReason::None))
    );
    // An unrecognized event must NOT become a false Waiting — that is the one
    // failure mode R11 exists to prevent.
    assert_eq!(classify("some.future.event", "{}"), None);
    // The type may arrive in the payload rather than the SSE event field.
    assert_eq!(
        agent_runtime::opencode_classify(&None, "{\"type\":\"session.idle\"}"),
        Some((AgentState::Idle, WaitReason::TurnComplete))
    );
}

#[test]
fn opencode_streams_normalized_signals_from_a_live_server() {
    let port = serve_once(EVENTS, true);
    let wt = tempfile::tempdir().unwrap();
    let rt = OpenCodeRuntime::with_endpoint(wt.path(), "127.0.0.1", port);

    assert_eq!(rt.kind(), AgentKind::OpenCode);
    assert_eq!(rt.signal_kind(), SignalKind::Sse);
    // permission.updated is a real blocked-on-you event, so this adapter may
    // raise an OS notification.
    assert_eq!(rt.fidelity(), WaitFidelity::Exact);

    let rx = rt.subscribe().expect("OpenCode produces its own stream");

    let running = rx.recv_timeout(RECV).unwrap();
    assert_eq!(running.state, AgentState::Running);
    assert_eq!(running.worktree_root, wt.path());
    assert_eq!(running.kind, AgentKind::OpenCode);

    let waiting = rx.recv_timeout(RECV).unwrap();
    assert_eq!(waiting.state, AgentState::Waiting);
    assert_eq!(waiting.reason, WaitReason::PermissionPrompt);
    assert!(
        waiting.should_notify(rt.fidelity()),
        "Exact + Waiting must notify"
    );

    let idle = rx.recv_timeout(RECV).unwrap();
    assert_eq!(idle.state, AgentState::Idle);
}

#[test]
fn opencode_needs_no_config_injection() {
    // Its server comes up alongside the TUI, so the plain "run the CLI" path
    // already enables the enhancement.
    let wt = tempfile::tempdir().unwrap();
    let rt = OpenCodeRuntime::new(wt.path());
    assert!(rt.on_spawn(wt.path()).is_ok());
}

#[test]
fn concurrent_opencode_launches_install_complete_shims() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();
    let ready = std::sync::Barrier::new(16);
    std::thread::scope(|scope| {
        for _ in 0..16 {
            scope.spawn(|| {
                let runtime = OpenCodeRuntime::new(path).with_state_dir(path);
                ready.wait();
                runtime.on_spawn(path).unwrap();
            });
        }
    });

    let shims = path.join("shims");
    let plugin = shims.join("opencode-remote");
    let package: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(plugin.join("package.json")).unwrap())
            .unwrap();
    assert_eq!(package["exports"]["./tui"], "./tui.js");
    assert_eq!(
        std::fs::read_to_string(plugin.join("tui.js")).unwrap(),
        include_str!("../shims/opencode-remote-tui.js"),
    );
    assert_eq!(
        std::fs::read_to_string(shims.join("opencode-remote-launch.mjs")).unwrap(),
        include_str!("../shims/opencode-remote-launch.mjs"),
    );
    assert_eq!(std::fs::read_dir(shims).unwrap().count(), 2);
    assert_eq!(std::fs::read_dir(plugin).unwrap().count(), 2);
}

// ---- Claude Code adapter ---------------------------------------------------

#[test]
fn claude_code_never_touches_the_tracked_settings_file() {
    // settings.json is committed by teams, and our hook embeds an absolute
    // path to a shim under ~/.openorchestra — machine-specific and wrong on
    // anyone else's checkout. settings.local.json is the untracked override.
    let state = tempfile::tempdir().unwrap();
    let wt = tempfile::tempdir().unwrap();
    ClaudeCodeRuntime::new(state.path())
        .on_spawn(wt.path())
        .unwrap();

    assert!(wt.path().join(".claude/settings.local.json").exists());
    assert!(
        !wt.path().join(".claude/settings.json").exists(),
        "the tracked settings file must be left alone"
    );
}

#[test]
fn adapters_declare_what_they_write_into_the_worktree() {
    // These feed `git exclude_locally`, so a path written but not declared
    // would silently dirty the user's repo.
    let state = tempfile::tempdir().unwrap();
    let wt = tempfile::tempdir().unwrap();

    let claude = ClaudeCodeRuntime::new(state.path());
    assert!(claude
        .ignored_paths()
        .contains(&".claude/settings.local.json"));
    assert!(claude.ignored_paths().contains(&".harness/"));

    let codex = CodexRuntime::new(state.path());
    assert!(codex.ignored_paths().contains(&".codex/"));

    // OpenCode and Kimi write nothing, and must not claim otherwise.
    assert!(OpenCodeRuntime::new(wt.path()).ignored_paths().is_empty());
    assert!(KimiRuntime.ignored_paths().is_empty());

    let grok = GrokRuntime::new(state.path());
    assert!(grok
        .ignored_paths()
        .contains(&".grok/hooks/openorchestra.json"));
    assert!(grok.ignored_paths().contains(&".harness/"));
}

#[test]
fn claude_code_writes_notification_hooks_pointing_at_the_shim() {
    let state = tempfile::tempdir().unwrap();
    let wt = tempfile::tempdir().unwrap();
    let rt = ClaudeCodeRuntime::new(state.path());
    rt.on_spawn(wt.path()).unwrap();

    assert!(rt.shim_path().exists(), "shim must be materialized");
    let settings: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(wt.path().join(".claude/settings.local.json")).unwrap(),
    )
    .unwrap();

    // Every lifecycle edge is hooked: without UserPromptSubmit/PreToolUse a
    // working agent looks IDLE for its entire turn, which is exactly the bug
    // this shape fixes. And no `matcher` fields: matchers filter tool names
    // (PreToolUse only) — a Notification entry carrying one never fires.
    for event in ["UserPromptSubmit", "PreToolUse", "Notification", "Stop"] {
        let entries = settings["hooks"][event].as_array().unwrap();
        assert_eq!(entries.len(), 1, "{event} must be hooked exactly once");
        assert!(
            entries[0].get("matcher").is_none(),
            "{event} must not carry a matcher"
        );
    }

    let command = settings["hooks"]["Notification"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert!(command.contains(&rt.shim_path().display().to_string()));
    // Normalizes through the shared status file, like pi.
    assert_eq!(rt.signal_kind(), SignalKind::StatusFile);
    assert_eq!(rt.fidelity(), WaitFidelity::Exact);
}

#[test]
fn claude_code_preserves_the_users_own_settings_and_hooks() {
    // Clobbering a project's settings would be a hostile thing for a harness
    // to do — even in the local file, which may hold the user's own hooks.
    let state = tempfile::tempdir().unwrap();
    let wt = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(wt.path().join(".claude")).unwrap();
    std::fs::write(
        wt.path().join(".claude/settings.local.json"),
        r#"{"model":"opus","hooks":{"Notification":[{"matcher":"mine","hooks":[{"type":"command","command":"echo user-hook"}]}]}}"#,
    )
    .unwrap();

    ClaudeCodeRuntime::new(state.path())
        .on_spawn(wt.path())
        .unwrap();

    let text = std::fs::read_to_string(wt.path().join(".claude/settings.local.json")).unwrap();
    let settings: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(settings["model"], "opus", "unrelated settings must survive");
    assert!(
        text.contains("echo user-hook"),
        "the user's own hook must survive"
    );
    assert!(text.contains("UserPromptSubmit"), "ours must be added");
}

#[test]
fn claude_code_on_spawn_is_idempotent() {
    // Repeated spawns must not stack duplicate hook entries.
    let state = tempfile::tempdir().unwrap();
    let wt = tempfile::tempdir().unwrap();
    let rt = ClaudeCodeRuntime::new(state.path());
    rt.on_spawn(wt.path()).unwrap();
    rt.on_spawn(wt.path()).unwrap();
    rt.on_spawn(wt.path()).unwrap();

    let settings: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(wt.path().join(".claude/settings.local.json")).unwrap(),
    )
    .unwrap();
    for event in ["UserPromptSubmit", "PreToolUse", "Notification", "Stop"] {
        assert_eq!(
            settings["hooks"][event].as_array().unwrap().len(),
            1,
            "{event}: one entry, not one per spawn"
        );
    }
}

/// The `command` strings across every hook group in `hooks.json[event]`.
fn hook_commands(hooks: &serde_json::Value, event: &str) -> Vec<String> {
    hooks["hooks"][event]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|group| group["hooks"].as_array())
        .flatten()
        .filter_map(|h| h["command"].as_str().map(str::to_owned))
        .collect()
}

#[test]
fn codex_migrates_off_the_legacy_notify_line() {
    // Pre-hooks installs left `notify = ["node", ".../codex-notify.mjs"]` in
    // config.toml. Now that the hook owns status, that line must go — including
    // any stray copy the old line-append bug buried inside a nested table — so
    // the two mechanisms don't both write status.json. Everything else stays.
    let _guard = CODEX_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let codex_home = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("HARNESS_CODEX_HOME", codex_home.path()) };
    let state = tempfile::tempdir().unwrap();
    let wt = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(codex_home.path()).unwrap();
    let legacy = state.path().join("shims").join("codex-notify.mjs");
    std::fs::write(
        codex_home.path().join("config.toml"),
        format!(
            "model = \"gpt-5\"\nnotify = [\"node\", \"{shim}\"]\n\n\
             [mcp_servers.node_repl.env]\nPATH = \"/usr/bin\"\nnotify = [\"node\", \"{shim}\"]\n",
            shim = legacy.display()
        ),
    )
    .unwrap();

    CodexRuntime::new(state.path()).on_spawn(wt.path()).unwrap();

    let text = std::fs::read_to_string(codex_home.path().join("config.toml")).unwrap();
    let doc: toml::Value = toml::from_str(&text).expect("config must stay parseable");
    assert!(doc.get("notify").is_none(), "root notify must be removed");
    assert!(
        doc["mcp_servers"]["node_repl"]["env"]
            .get("notify")
            .is_none(),
        "the stray nested notify must be removed too"
    );
    assert_eq!(
        doc["model"].as_str(),
        Some("gpt-5"),
        "unrelated config survives"
    );
    assert_eq!(
        doc["mcp_servers"]["node_repl"]["env"]["PATH"].as_str(),
        Some("/usr/bin")
    );
}

// ---- Codex + Kimi: honest degradation (R11) --------------------------------

#[test]
fn codex_is_best_effort_and_may_not_claim_input_is_needed() {
    // Codex's TUI has no approval-blocked event — only turn start/stop. So its
    // status hook can say running/idle but never "input needed"; reporting the
    // latter is precisely the lie R11 forbids.
    let state = tempfile::tempdir().unwrap();
    let rt = CodexRuntime::new(state.path());
    assert_eq!(rt.fidelity(), WaitFidelity::BestEffort);
    // The hook writes the shared status.json, same transport as pi / Claude.
    assert_eq!(rt.signal_kind(), SignalKind::StatusFile);

    let waiting = agent_runtime::AgentSignal {
        worktree_root: Path::new("/wt").into(),
        tab_id: None,
        kind: AgentKind::Codex,
        state: AgentState::Waiting,
        reason: WaitReason::PermissionPrompt,
        session_file: None,
        tool: None,
    };
    assert!(!waiting.should_notify(rt.fidelity()));
}

#[test]
fn codex_installs_status_hooks_and_stays_idempotent() {
    let _guard = CODEX_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let codex_home = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("HARNESS_CODEX_HOME", codex_home.path()) };
    let state = tempfile::tempdir().unwrap();
    let wt = tempfile::tempdir().unwrap();
    let rt = CodexRuntime::new(state.path());
    rt.on_spawn(wt.path()).unwrap();
    rt.on_spawn(wt.path()).unwrap();

    assert!(
        rt.shim_path().exists(),
        "the hook shim must be materialized"
    );
    let shim = rt.shim_path().display().to_string();
    let hooks: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(codex_home.path().join("hooks.json")).unwrap(),
    )
    .unwrap();

    // Both turn edges installed, and running on_spawn twice must not stack them.
    for event in ["UserPromptSubmit", "Stop"] {
        let cmds = hook_commands(&hooks, event);
        assert_eq!(cmds.len(), 1, "one {event} hook, got: {cmds:?}");
        assert!(cmds[0].contains(&shim), "{event} hook must call our shim");
    }
}

#[test]
fn codex_keeps_the_hook_index_used_for_trust() {
    let _guard = CODEX_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let codex_home = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("HARNESS_CODEX_HOME", codex_home.path()) };
    let state = tempfile::tempdir().unwrap();
    let wt = tempfile::tempdir().unwrap();
    let rt = CodexRuntime::new(state.path());
    let cmd = format!("node {:?}", rt.shim_path().to_string_lossy());
    let hooks = serde_json::json!({
        "hooks": {
            "UserPromptSubmit": [
                { "hooks": [{ "type": "command", "command": cmd }, { "type": "command", "command": "first-user-hook" }] },
                { "hooks": [{ "type": "command", "command": "later-user-hook" }] },
            ],
            "Stop": [{ "hooks": [{ "type": "command", "command": cmd }] }],
        },
    });
    let path = codex_home.path().join("hooks.json");
    let original = hooks.to_string();
    std::fs::write(&path, &original).unwrap();

    rt.on_spawn(wt.path()).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
}

#[test]
fn auto_injects_cursor_hooks_without_requiring_a_launch_command() {
    let cursor_home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let wt = tempfile::tempdir().unwrap();
    let hooks = cursor_home.path().join("hooks.json");
    std::fs::write(
        &hooks,
        r#"{"version":1,"hooks":{"stop":[{"command":"my-existing-hook"}]}}"#,
    )
    .unwrap();
    unsafe { std::env::set_var("HARNESS_CURSOR_HOME", cursor_home.path()) };
    let runtime = ConfigurableRuntime::new(
        AgentKind::Other("cursor".into()),
        state.path(),
        SignalProfile::best_effort(HookInstall::Auto),
    );
    runtime.on_spawn(wt.path()).unwrap();
    runtime.on_spawn(wt.path()).unwrap();
    unsafe { std::env::remove_var("HARNESS_CURSOR_HOME") };
    let config: serde_json::Value = serde_json::from_slice(&std::fs::read(hooks).unwrap()).unwrap();
    assert_eq!(config["hooks"]["stop"][0]["command"], "my-existing-hook");
    assert_eq!(config["hooks"]["stop"].as_array().unwrap().len(), 2);
    assert_eq!(
        config["hooks"]["beforeSubmitPrompt"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(config["hooks"]["beforeSubmitPrompt"][0]["command"]
        .as_str()
        .unwrap()
        .contains("harness-status.mjs"));
    assert_eq!(
        HookInstall::Auto.effective("arbitrary-agent"),
        HookInstall::None
    );
}

#[test]
fn codex_preserves_the_users_own_hooks() {
    let _guard = CODEX_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let codex_home = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("HARNESS_CODEX_HOME", codex_home.path()) };
    // The user already runs their own hooks. Ours must join, not replace: a
    // foreign UserPromptSubmit hook has to survive alongside ours, and an
    // unrelated event (SessionStart) must be left entirely alone.
    let state = tempfile::tempdir().unwrap();
    let wt = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(codex_home.path()).unwrap();
    std::fs::write(
        codex_home.path().join("hooks.json"),
        r#"{ "hooks": {
              "SessionStart": [ { "hooks": [ { "type": "command", "command": "/usr/local/bin/mine.sh" } ] } ],
              "UserPromptSubmit": [ { "hooks": [ { "type": "command", "command": "/usr/local/bin/mine.sh" } ] } ]
        } }"#,
    )
    .unwrap();

    CodexRuntime::new(state.path()).on_spawn(wt.path()).unwrap();

    let hooks: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(codex_home.path().join("hooks.json")).unwrap(),
    )
    .unwrap();
    // The user's SessionStart hook is untouched.
    assert_eq!(
        hook_commands(&hooks, "SessionStart"),
        vec!["/usr/local/bin/mine.sh"]
    );
    // Their UserPromptSubmit hook survives, and ours is added beside it.
    let ups = hook_commands(&hooks, "UserPromptSubmit");
    assert!(
        ups.iter().any(|c| c == "/usr/local/bin/mine.sh"),
        "foreign hook kept: {ups:?}"
    );
    assert!(
        ups.iter().any(|c| c.contains("codex-hook.mjs")),
        "our hook added: {ups:?}"
    );
    assert_eq!(ups.len(), 2, "exactly the two, no clobber: {ups:?}");
}

#[test]
fn grok_writes_its_own_hook_file_and_not_claude_settings() {
    let state = tempfile::tempdir().unwrap();
    let wt = tempfile::tempdir().unwrap();
    let rt = GrokRuntime::new(state.path());
    rt.on_spawn(wt.path()).unwrap();

    assert!(rt.shim_path().exists(), "shim must be materialized");
    assert!(
        state.path().join("shims/claude-code-status.mjs").exists(),
        "the grok shim imports the shared classifier"
    );
    let hook = wt.path().join(".grok/hooks/openorchestra.json");
    assert!(hook.exists());
    assert!(
        !wt.path().join(".claude/settings.local.json").exists(),
        "grok must not write Claude's settings"
    );

    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&hook).unwrap()).unwrap();
    for event in ["UserPromptSubmit", "PreToolUse", "Notification", "Stop"] {
        let entries = settings["hooks"][event].as_array().unwrap();
        assert_eq!(entries.len(), 1, "{event} must be hooked exactly once");
        assert!(
            entries[0].get("matcher").is_none(),
            "{event} must not carry a matcher"
        );
    }
    let command = settings["hooks"]["Notification"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert!(command.contains(&rt.shim_path().display().to_string()));
    assert_eq!(rt.signal_kind(), SignalKind::StatusFile);
    assert_eq!(rt.fidelity(), WaitFidelity::Exact);
    assert_eq!(rt.kind(), AgentKind::Grok);
}

#[test]
fn grok_preserves_other_hooks_in_its_file_and_stays_idempotent() {
    let state = tempfile::tempdir().unwrap();
    let wt = tempfile::tempdir().unwrap();
    let dir = wt.path().join(".grok/hooks");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("openorchestra.json"),
        r#"{"note":"keep","hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"echo user-hook"}]}]}}"#,
    )
    .unwrap();

    let rt = GrokRuntime::new(state.path());
    rt.on_spawn(wt.path()).unwrap();
    rt.on_spawn(wt.path()).unwrap();

    let text = std::fs::read_to_string(dir.join("openorchestra.json")).unwrap();
    let settings: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(settings["note"], "keep");
    assert!(text.contains("echo user-hook"));
    for event in ["UserPromptSubmit", "PreToolUse", "Notification", "Stop"] {
        assert_eq!(
            settings["hooks"][event].as_array().unwrap().len(),
            1,
            "{event}: one entry, not one per spawn"
        );
    }
}

#[test]
fn grok_shim_maps_task_complete_to_idle_and_stamps_grok() {
    let shim = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("shims/grok-status.mjs");
    let url = format!("file://{}", shim.display());
    let check = std::process::Command::new("node")
        .arg("--input-type=module")
        .arg("-e")
        .arg(format!(
            r#"import {{ classify }} from "{url}";
const cases = [
  [{{hook_event_name:"Notification", notification_type:"task_complete"}}, "idle"],
  [{{hook_event_name:"Notification", notification_type:"permission_prompt"}}, "waiting"],
  [{{hook_event_name:"Stop"}}, "idle"],
  [{{hook_event_name:"UserPromptSubmit"}}, "running"],
];
for (const [payload, want] of cases) {{
  const got = classify(payload);
  if (got !== want) {{
    console.error(JSON.stringify(payload), got, "!=", want);
    process.exit(1);
  }}
}}"#
        ))
        .output();
    match check {
        Ok(out) => assert!(
            out.status.success(),
            "classify: {}",
            String::from_utf8_lossy(&out.stderr)
        ),
        Err(_) => {
            eprintln!("node not installed — skipping grok shim check");
            return;
        }
    }

    let wt = tempfile::tempdir().unwrap();
    let mut child = std::process::Command::new("node")
        .arg(&shim)
        .current_dir(wt.path())
        .env_remove("HARNESS_AGENT_WORKTREE")
        .env_remove("HARNESS_AGENT_TAB_ID")
        .env_remove("HARNESS_REMOTE_SESSION")
        .env_remove("TMUX_PANE")
        .env_remove("CLAUDE_PROJECT_DIR")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("node");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"hook_event_name":"Notification","notification_type":"task_complete"}"#)
        .unwrap();
    let finished = child.wait_with_output().unwrap();
    assert!(
        finished.status.success(),
        "shim: {}",
        String::from_utf8_lossy(&finished.stderr)
    );
    let status: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(wt.path().join(".harness/status.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(status["agent"], "grok");
    assert_eq!(status["state"], "idle");
}

#[test]
fn kimi_is_terminal_only_and_raises_nothing() {
    let rt = KimiRuntime;
    assert_eq!(rt.kind(), AgentKind::Kimi);
    assert_eq!(rt.fidelity(), WaitFidelity::TerminalOnly);
    assert_eq!(rt.signal_kind(), SignalKind::None);
    assert!(rt.subscribe().is_none());
    // Nothing to configure — and that must not be an error.
    let wt = tempfile::tempdir().unwrap();
    assert!(rt.on_spawn(wt.path()).is_ok());
}

// ---- dispatch --------------------------------------------------------------

#[test]
fn runtime_for_dispatches_every_known_agent_to_its_adapter() {
    let state = tempfile::tempdir().unwrap();
    let wt = tempfile::tempdir().unwrap();
    for kind in AgentKind::known() {
        let rt = agent_runtime::runtime_for(&kind, wt.path(), state.path());
        assert_eq!(rt.kind(), kind, "runtime_for returned the wrong adapter");
    }
}

#[test]
fn every_adapters_on_spawn_is_safe_to_call() {
    // on_spawn runs on every spawn and must never be the reason an agent
    // fails to start. Codex's on_spawn edits $HARNESS_CODEX_HOME — hold the
    // lock and point it at a tempdir, or this test races the other codex
    // tests and, unset, would edit the REAL ~/.codex.
    let _guard = CODEX_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let codex_home = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("HARNESS_CODEX_HOME", codex_home.path()) };
    let state = tempfile::tempdir().unwrap();
    for kind in AgentKind::known() {
        let wt = tempfile::tempdir().unwrap();
        let rt = agent_runtime::runtime_for(&kind, wt.path(), state.path());
        assert!(rt.on_spawn(wt.path()).is_ok(), "{kind} on_spawn failed");
    }
}

// The shim scripts are executed by node, not Rust, so assert they at least
// parse — a syntax error would only surface as a silently dead status pill.
#[test]
fn shims_are_syntactically_valid_javascript() {
    let _guard = CODEX_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let codex_home = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("HARNESS_CODEX_HOME", codex_home.path()) };
    let state = tempfile::tempdir().unwrap();
    let wt = tempfile::tempdir().unwrap();
    ClaudeCodeRuntime::new(state.path())
        .on_spawn(wt.path())
        .unwrap();
    GrokRuntime::new(state.path()).on_spawn(wt.path()).unwrap();
    CodexRuntime::new(state.path()).on_spawn(wt.path()).unwrap();

    for shim in [
        ClaudeCodeRuntime::new(state.path()).shim_path(),
        GrokRuntime::new(state.path()).shim_path(),
        CodexRuntime::new(state.path()).shim_path(),
        agent_runtime::harness_status_shim_path(state.path()),
    ] {
        if !shim.exists() {
            agent_runtime::materialize_harness_status_shim(state.path()).unwrap();
        }
        let out = std::process::Command::new("node")
            .arg("--check")
            .arg(&shim)
            .output();
        match out {
            Ok(out) => assert!(
                out.status.success(),
                "{} failed node --check: {}",
                shim.display(),
                String::from_utf8_lossy(&out.stderr)
            ),
            Err(_) => eprintln!("node not installed — skipping shim syntax check"),
        }
    }
}

// ---- configurable custom agents -------------------------------------------

#[test]
fn configurable_runtime_installs_claude_local_hooks_with_agent_env() {
    let state = tempfile::tempdir().unwrap();
    let wt = tempfile::tempdir().unwrap();
    let kind = AgentKind::Other("cursor-agent".into());
    let profile = SignalProfile::exact(HookInstall::ClaudeLocalSettings);
    let rt = ConfigurableRuntime::new(kind.clone(), state.path(), profile);
    assert_eq!(rt.fidelity(), WaitFidelity::Exact);
    assert_eq!(rt.signal_kind(), SignalKind::StatusFile);
    rt.on_spawn(wt.path()).unwrap();

    assert!(
        rt.shim_path().exists(),
        "universal shim must be materialized"
    );
    let settings: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(wt.path().join(".claude/settings.local.json")).unwrap(),
    )
    .unwrap();
    let shim = rt.shim_path().display().to_string();
    for event in ["UserPromptSubmit", "PreToolUse", "Notification", "Stop"] {
        let blob = settings["hooks"][event].to_string();
        assert!(
            blob.contains(&shim),
            "{event} must call universal shim: {blob}"
        );
        assert!(
            blob.contains("harness-status.mjs"),
            "{event} must use harness-status.mjs"
        );
    }
    let cmd = rt.command(wt.path()).unwrap();
    assert!(
        cmd.env
            .iter()
            .any(|(k, v)| k == "HARNESS_MANAGED" && v == "1"),
        "managed marker required: {:?}",
        cmd.env
    );
    assert!(
        cmd.env
            .iter()
            .any(|(k, v)| k == "HARNESS_AGENT" && v == "cursor-agent"),
        "agent slug in env: {:?}",
        cmd.env
    );
}

#[test]
fn configurable_runtime_codex_hooks_are_idempotent() {
    let _guard = CODEX_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let codex_home = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("HARNESS_CODEX_HOME", codex_home.path()) };

    let state = tempfile::tempdir().unwrap();
    let wt = tempfile::tempdir().unwrap();
    let kind = AgentKind::Other("my-agent".into());
    let profile = SignalProfile::best_effort(HookInstall::CodexUserHooks);
    let rt = ConfigurableRuntime::new(kind, state.path(), profile);
    rt.on_spawn(wt.path()).unwrap();
    rt.on_spawn(wt.path()).unwrap(); // idempotent

    let hooks: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(codex_home.path().join("hooks.json")).unwrap(),
    )
    .unwrap();
    let shim = rt.shim_path().display().to_string();
    for event in ["UserPromptSubmit", "Stop"] {
        let cmds = hook_commands(&hooks, event);
        assert_eq!(
            cmds.iter().filter(|c| c.contains(&shim)).count(),
            1,
            "{event} must have exactly one of our hooks: {cmds:?}"
        );
        let ours = cmds.iter().find(|c| c.contains(&shim)).unwrap();
        assert!(
            ours.contains("HARNESS_AGENT") && ours.contains("my-agent"),
            "{event} hook must gate on HARNESS_AGENT=my-agent: {ours}"
        );
    }
}

#[test]
fn configurable_codex_hooks_coexist_with_builtin_codex_hook() {
    let _guard = CODEX_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let codex_home = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("HARNESS_CODEX_HOME", codex_home.path()) };

    let state = tempfile::tempdir().unwrap();
    let wt = tempfile::tempdir().unwrap();

    // Built-in Codex installs first…
    CodexRuntime::new(state.path()).on_spawn(wt.path()).unwrap();
    // …then a custom agent with the Codex dialect.
    let custom = ConfigurableRuntime::new(
        AgentKind::Other("cursor-agent".into()),
        state.path(),
        SignalProfile::best_effort(HookInstall::CodexUserHooks),
    );
    custom.on_spawn(wt.path()).unwrap();

    let hooks: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(codex_home.path().join("hooks.json")).unwrap(),
    )
    .unwrap();
    for event in ["UserPromptSubmit", "Stop"] {
        let cmds = hook_commands(&hooks, event);
        assert!(
            cmds.iter().any(|c| c.contains("codex-hook.mjs")),
            "{event} must still have the built-in codex hook: {cmds:?}"
        );
        assert!(
            cmds.iter()
                .any(|c| c.contains("harness-status.mjs") && c.contains("cursor-agent")),
            "{event} must have the gated custom hook: {cmds:?}"
        );
        assert_eq!(cmds.len(), 2, "{event} must keep both entries: {cmds:?}");
    }
}

#[test]
fn runtime_for_with_signal_selects_configurable_for_other() {
    let state = tempfile::tempdir().unwrap();
    let wt = tempfile::tempdir().unwrap();
    let kind = AgentKind::Other("cursor-agent".into());

    let plain = agent_runtime::runtime_for(&kind, wt.path(), state.path());
    assert_eq!(plain.fidelity(), WaitFidelity::TerminalOnly);

    let signaled = agent_runtime::runtime_for_with_signal(
        &kind,
        wt.path(),
        state.path(),
        Some(SignalProfile::exact(HookInstall::None)),
    );
    assert_eq!(signaled.fidelity(), WaitFidelity::Exact);
    assert_eq!(signaled.signal_kind(), SignalKind::StatusFile);
}

#[test]
fn other_with_exact_fidelity_notifies_on_waiting() {
    let signal = AgentSignal {
        worktree_root: PathBuf::from("/wt"),
        tab_id: None,
        kind: AgentKind::Other("cursor-agent".into()),
        state: AgentState::Waiting,
        reason: WaitReason::PermissionPrompt,
        session_file: None,
        tool: None,
    };
    assert!(signal.should_notify(WaitFidelity::Exact));
    assert!(!signal.should_notify(WaitFidelity::BestEffort));
    assert!(!signal.should_notify(WaitFidelity::TerminalOnly));

    let finished = AgentSignal {
        state: AgentState::Idle,
        reason: WaitReason::TurnComplete,
        ..signal.clone()
    };
    assert!(finished.should_notify_finished(Some(AgentState::Running), WaitFidelity::BestEffort));
    assert!(finished.should_notify_finished(Some(AgentState::Running), WaitFidelity::Exact));
    assert!(!finished.should_notify_finished(Some(AgentState::Running), WaitFidelity::TerminalOnly));
}

#[test]
fn harness_status_shim_writes_agent_field_when_managed() {
    let state = tempfile::tempdir().unwrap();
    let wt = tempfile::tempdir().unwrap();
    let shim = agent_runtime::materialize_harness_status_shim(state.path()).unwrap();

    let out = std::process::Command::new("node")
        .arg(&shim)
        .arg("waiting")
        .current_dir(wt.path())
        .env("HARNESS_MANAGED", "1")
        .env("HARNESS_AGENT", "cursor-agent")
        .env_remove("CLAUDE_PROJECT_DIR")
        .env_remove("CODEX_PROJECT_DIR")
        .output();
    let Ok(out) = out else {
        eprintln!("node not installed — skipping shim write test");
        return;
    };
    assert!(
        out.status.success(),
        "shim stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let status: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(wt.path().join(".harness/status.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(status["state"], "waiting");
    assert_eq!(status["agent"], "cursor-agent");
}

#[test]
fn harness_status_shim_refuses_to_write_without_managed_marker() {
    let state = tempfile::tempdir().unwrap();
    let wt = tempfile::tempdir().unwrap();
    let shim = agent_runtime::materialize_harness_status_shim(state.path()).unwrap();

    let out = std::process::Command::new("node")
        .arg(&shim)
        .arg("running")
        .current_dir(wt.path())
        .env_remove("HARNESS_MANAGED")
        .env("HARNESS_AGENT", "cursor-agent")
        .output();
    let Ok(out) = out else {
        eprintln!("node not installed — skipping shim gate test");
        return;
    };
    assert!(out.status.success());
    assert!(
        !wt.path().join(".harness/status.json").exists(),
        "unmanaged runs must not write status.json"
    );
}

#[test]
fn harness_status_shim_refuses_to_write_without_agent_slug() {
    // Built-in Codex sets HARNESS_MANAGED alone; without HARNESS_AGENT this
    // shim must stay silent so a leftover ~/.codex entry cannot race codex-hook.
    let state = tempfile::tempdir().unwrap();
    let wt = tempfile::tempdir().unwrap();
    let shim = agent_runtime::materialize_harness_status_shim(state.path()).unwrap();

    let out = std::process::Command::new("node")
        .arg(&shim)
        .arg("running")
        .current_dir(wt.path())
        .env("HARNESS_MANAGED", "1")
        .env_remove("HARNESS_AGENT")
        .output();
    let Ok(out) = out else {
        eprintln!("node not installed — skipping shim agent-gate test");
        return;
    };
    assert!(out.status.success());
    assert!(
        !wt.path().join(".harness/status.json").exists(),
        "managed-but-no-agent must not write status.json"
    );
}

#[test]
fn status_shim_ignores_inherited_tab_env_outside_its_worktree() {
    // A nested agent inherits the parent tab's env. Running in another
    // worktree, it must report there, untagged, and leave the parent alone.
    let state = tempfile::tempdir().unwrap();
    let parent = tempfile::tempdir().unwrap();
    let nested = tempfile::tempdir().unwrap();
    let shim = agent_runtime::materialize_harness_status_shim(state.path()).unwrap();
    let run = |cwd: &std::path::Path| {
        std::process::Command::new("node")
            .arg(&shim)
            .arg("running")
            .current_dir(cwd)
            .env("HARNESS_MANAGED", "1")
            .env("HARNESS_AGENT", "cursor-agent")
            .env("HARNESS_AGENT_WORKTREE", parent.path())
            .env("HARNESS_AGENT_TAB_ID", "7")
            .env_remove("HARNESS_REMOTE_SESSION")
            .env_remove("CLAUDE_PROJECT_DIR")
            .env_remove("CODEX_PROJECT_DIR")
            .output()
    };
    let Ok(out) = run(nested.path()) else {
        eprintln!("node not installed — skipping inherited tab env test");
        return;
    };
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(
        !parent.path().join(".harness").exists(),
        "nested agent wrote into the parent tab's worktree"
    );
    let status: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(nested.path().join(".harness/status.json")).unwrap(),
    )
    .unwrap();
    assert!(status.get("tabId").is_none(), "nested status kept the parent tab id");
    assert!(!nested.path().join(".harness/status-tab-7.json").exists());

    // A subdirectory of the tab's own worktree still reports as that tab.
    let sub = parent.path().join("src");
    std::fs::create_dir(&sub).unwrap();
    let out = run(&sub).unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let tagged: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(parent.path().join(".harness/status-tab-7.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(tagged["tabId"], 7);
    assert!(!sub.join(".harness").exists());
}

#[test]
fn harness_status_shim_uses_codex_project_dir_over_cwd() {
    let state = tempfile::tempdir().unwrap();
    let launch_cwd = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let shim = agent_runtime::materialize_harness_status_shim(state.path()).unwrap();

    let out = std::process::Command::new("node")
        .arg(&shim)
        .arg("running")
        .current_dir(launch_cwd.path())
        .env("HARNESS_MANAGED", "1")
        .env("HARNESS_AGENT", "my-agent")
        .env("CODEX_PROJECT_DIR", project.path())
        .env_remove("CLAUDE_PROJECT_DIR")
        .output();
    let Ok(out) = out else {
        eprintln!("node not installed — skipping CODEX_PROJECT_DIR test");
        return;
    };
    assert!(
        out.status.success(),
        "shim stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        project.path().join(".harness/status.json").exists(),
        "status must land in CODEX_PROJECT_DIR"
    );
    assert!(
        !launch_cwd.path().join(".harness/status.json").exists(),
        "must not write to the hook process cwd"
    );
}

#[test]
fn harness_status_shim_uses_payload_cwd_for_codex_hooks() {
    let state = tempfile::tempdir().unwrap();
    let launch_cwd = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let shim = agent_runtime::materialize_harness_status_shim(state.path()).unwrap();

    let payload = serde_json::json!({
        "hook_event_name": "UserPromptSubmit",
        "cwd": project.path().display().to_string(),
    });
    let child = std::process::Command::new("node")
        .arg(&shim)
        .current_dir(launch_cwd.path())
        .env("HARNESS_MANAGED", "1")
        .env("HARNESS_AGENT", "my-agent")
        .env_remove("CLAUDE_PROJECT_DIR")
        .env_remove("CODEX_PROJECT_DIR")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn();
    let Ok(mut child) = child else {
        eprintln!("node not installed — skipping payload.cwd test");
        return;
    };
    use std::io::Write;
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "shim stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let status: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.path().join(".harness/status.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(status["state"], "running");
    assert_eq!(status["agent"], "my-agent");
    assert!(
        !launch_cwd.path().join(".harness/status.json").exists(),
        "must not write to the hook process cwd"
    );
}

/// PreToolUse carries the tool about to run into status.json — trimmed to a
/// few short fields — so the conductor can say "Editing main.rs" while the
/// call is in flight. Every other event clears it.
#[test]
fn claude_code_shim_records_the_tool_about_to_run() {
    let wt = tempfile::tempdir().unwrap();
    let shim = Path::new(env!("CARGO_MANIFEST_DIR")).join("shims/claude-code-status.mjs");
    let run = |payload: serde_json::Value| {
        use std::io::Write;
        let mut child = std::process::Command::new("node")
            .arg(&shim)
            .env("CLAUDE_PROJECT_DIR", wt.path())
            .stdin(std::process::Stdio::piped())
            .spawn()
            .ok()?;
        child
            .stdin
            .take()?
            .write_all(payload.to_string().as_bytes())
            .ok()?;
        child.wait().ok()?;
        let text = std::fs::read_to_string(wt.path().join(".harness/status.json")).ok()?;
        serde_json::from_str::<serde_json::Value>(&text).ok()
    };
    let long = "x".repeat(5000);
    let Some(status) = run(serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Edit",
        "tool_use_id": "toolu_1",
        "transcript_path": "/t.jsonl",
        "tool_input": { "file_path": "/src/main.rs", "old_string": long, "new_string": "y" }
    })) else {
        eprintln!("node not installed — skipping shim tool test");
        return;
    };
    assert_eq!(status["state"], "running");
    assert_eq!(status["tool"]["name"], "Edit");
    assert_eq!(status["tool"]["id"], "toolu_1");
    assert_eq!(status["tool"]["input"]["file_path"], "/src/main.rs");
    assert!(
        status["tool"]["input"].get("old_string").is_none(),
        "file contents never land on disk"
    );

    let status = run(serde_json::json!({ "hook_event_name": "Stop" })).unwrap();
    assert_eq!(status["state"], "idle");
    assert!(status["tool"].is_null());
}

/// Run both Claude-shaped shims' `classify` on one hook payload via node.
/// `None` when node is not installed.
fn shim_classify(shim: &str, payload: &str) -> Option<String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("shims")
        .join(shim);
    let script = format!(
        "import {{ classify }} from {:?}; process.stdout.write(classify(JSON.parse(process.argv[1])));",
        format!("file://{}", path.display())
    );
    let out = std::process::Command::new("node")
        .args(["--input-type=module", "-e", &script, payload])
        .output()
        .ok()?;
    assert!(
        out.status.success(),
        "node stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(String::from_utf8(out.stdout).unwrap())
}

// Claude Code sends an `idle_prompt` Notification ("Claude is waiting for your
// input") 60s after a turn ends. That is a reminder, not a block: mapping it
// to `waiting` left the pill stuck on WAITING after the user had already seen
// the finished turn, until they typed again. Only permission/approval prompts
// are waiting.
#[test]
fn claude_idle_reminder_is_idle_but_permission_prompt_waits() {
    let cases = [
        (
            r#"{"hook_event_name":"Notification","notification_type":"idle_prompt","message":"Claude is waiting for your input"}"#,
            "idle",
        ),
        (
            r#"{"hook_event_name":"Notification","message":"Claude is waiting for your input"}"#,
            "idle",
        ),
        (
            r#"{"hook_event_name":"Notification","notification_type":"permission_prompt","message":"Claude needs your permission to use Bash"}"#,
            "waiting",
        ),
        (
            r#"{"hook_event_name":"Notification","message":"Claude needs your approval"}"#,
            "waiting",
        ),
        (r#"{"hook_event_name":"Stop"}"#, "idle"),
        (r#"{"hook_event_name":"UserPromptSubmit"}"#, "running"),
    ];
    for shim in ["claude-code-status.mjs", "harness-status.mjs"] {
        for (payload, want) in cases {
            let Some(got) = shim_classify(shim, payload) else {
                eprintln!("node not installed — skipping shim classify test");
                return;
            };
            assert_eq!(got, want, "{shim}: {payload}");
        }
    }
}
