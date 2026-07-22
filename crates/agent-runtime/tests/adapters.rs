//! A2–A4 tests — the peer adapters (architecture §4.5).
//!
//! The OpenCode tests stand up a real TCP server speaking real HTTP/1.1 + SSE,
//! so the hand-rolled client in `sse.rs` is exercised against actual bytes
//! rather than a mock of itself.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;
use std::time::Duration;

/// Both codex tests point HARNESS_CODEX_HOME at their own tempdir; the
/// variable is process-global, so they must not run concurrently.
static CODEX_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

use agent_runtime::sse::SseStream;
use agent_runtime::{
    AgentKind, AgentRuntime, AgentState, ClaudeCodeRuntime, CodexRuntime, KimiRuntime,
    OpenCodeRuntime, SignalKind, WaitFidelity, WaitReason,
};

const RECV: Duration = Duration::from_secs(10);

/// Count `notify = ...` lines in a Codex config.
///
/// Substring-counting "notify" would also match the shim's own filename
/// (`codex-notify.mjs`), which is exactly the false positive this avoids.
fn notify_lines(config: &str) -> usize {
    config
        .lines()
        .filter(|l| l.trim_start().starts_with("notify"))
        .count()
}

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
    let events: Vec<_> = SseStream::connect("127.0.0.1", port, "/event").unwrap().collect();
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
    let events: Vec<_> = SseStream::connect("127.0.0.1", port, "/event").unwrap().collect();
    assert_eq!(events.len(), 3, "{events:#?}");
    assert_eq!(events[1].event.as_deref(), Some("permission.updated"));
}

#[test]
fn sse_client_joins_multiline_data_and_skips_comments() {
    let port = serve_once(": keep-alive\ndata: one\ndata: two\n\nid: 7\ndata: three\n\n", false);
    let events: Vec<_> = SseStream::connect("127.0.0.1", port, "/event").unwrap().collect();
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
    assert!(waiting.should_notify(rt.fidelity()), "Exact + Waiting must notify");

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

// ---- Claude Code adapter ---------------------------------------------------

#[test]
fn claude_code_never_touches_the_tracked_settings_file() {
    // settings.json is committed by teams, and our hook embeds an absolute
    // path to a shim under ~/.openorchestra — machine-specific and wrong on
    // anyone else's checkout. settings.local.json is the untracked override.
    let state = tempfile::tempdir().unwrap();
    let wt = tempfile::tempdir().unwrap();
    ClaudeCodeRuntime::new(state.path()).on_spawn(wt.path()).unwrap();

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
    assert!(claude.ignored_paths().contains(&".claude/settings.local.json"));
    assert!(claude.ignored_paths().contains(&".harness/"));

    let codex = CodexRuntime::new(state.path());
    assert!(codex.ignored_paths().contains(&".codex/"));

    // OpenCode and Kimi write nothing, and must not claim otherwise.
    assert!(OpenCodeRuntime::new(wt.path()).ignored_paths().is_empty());
    assert!(KimiRuntime.ignored_paths().is_empty());
}

#[test]
fn claude_code_writes_notification_hooks_pointing_at_the_shim() {
    let state = tempfile::tempdir().unwrap();
    let wt = tempfile::tempdir().unwrap();
    let rt = ClaudeCodeRuntime::new(state.path());
    rt.on_spawn(wt.path()).unwrap();

    assert!(rt.shim_path().exists(), "shim must be materialized");
    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(wt.path().join(".claude/settings.local.json")).unwrap())
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

    ClaudeCodeRuntime::new(state.path()).on_spawn(wt.path()).unwrap();

    let text = std::fs::read_to_string(wt.path().join(".claude/settings.local.json")).unwrap();
    let settings: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(settings["model"], "opus", "unrelated settings must survive");
    assert!(text.contains("echo user-hook"), "the user's own hook must survive");
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

    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(wt.path().join(".claude/settings.local.json")).unwrap())
            .unwrap();
    for event in ["UserPromptSubmit", "PreToolUse", "Notification", "Stop"] {
        assert_eq!(
            settings["hooks"][event].as_array().unwrap().len(),
            1,
            "{event}: one entry, not one per spawn"
        );
    }
}

// ---- Codex + Kimi: honest degradation (R11) --------------------------------

#[test]
fn codex_is_best_effort_and_may_not_claim_input_is_needed() {
    // Codex's TUI has no approval-blocked event — only agent-turn-complete.
    // Reporting that as "input needed" is precisely the lie R11 forbids.
    let state = tempfile::tempdir().unwrap();
    let rt = CodexRuntime::new(state.path());
    assert_eq!(rt.fidelity(), WaitFidelity::BestEffort);
    assert_eq!(rt.signal_kind(), SignalKind::Notify);

    let waiting = agent_runtime::AgentSignal {
        worktree_root: Path::new("/wt").into(),
        kind: AgentKind::Codex,
        state: AgentState::Waiting,
        reason: WaitReason::PermissionPrompt,
        session_file: None,
    };
    assert!(!waiting.should_notify(rt.fidelity()));
}

#[test]
fn codex_writes_a_notify_line_and_stays_idempotent() {
    let _guard = CODEX_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let codex_home = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("HARNESS_CODEX_HOME", codex_home.path()) };
    let state = tempfile::tempdir().unwrap();
    let wt = tempfile::tempdir().unwrap();
    let rt = CodexRuntime::new(state.path());
    rt.on_spawn(wt.path()).unwrap();
    rt.on_spawn(wt.path()).unwrap();

    let config = std::fs::read_to_string(codex_home.path().join("config.toml")).unwrap();
    assert_eq!(
        notify_lines(&config),
        1,
        "exactly one notify line, got: {config}"
    );
    assert!(config.contains(&rt.shim_path().display().to_string()));
    assert!(rt.shim_path().exists());
}

#[test]
fn codex_preserves_unrelated_config_but_takes_over_notify() {
    let _guard = CODEX_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let codex_home = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("HARNESS_CODEX_HOME", codex_home.path()) };
    // Codex honors a single `notify`, so ours has to win or the pill silently
    // never updates — but everything else in the file must survive.
    let state = tempfile::tempdir().unwrap();
    let wt = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(codex_home.path()).unwrap();
    std::fs::write(
        codex_home.path().join("config.toml"),
        "model = \"gpt-5\"\nnotify = [\"/usr/bin/say\", \"done\"]\napproval_policy = \"on-request\"\n",
    )
    .unwrap();

    CodexRuntime::new(state.path()).on_spawn(wt.path()).unwrap();

    let config = std::fs::read_to_string(codex_home.path().join("config.toml")).unwrap();
    assert!(config.contains("model = \"gpt-5\""));
    assert!(config.contains("approval_policy"));
    assert!(!config.contains("/usr/bin/say"), "the foreign notify must be replaced");
    assert_eq!(notify_lines(&config), 1);
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
    // fails to start.
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
    let state = tempfile::tempdir().unwrap();
    let wt = tempfile::tempdir().unwrap();
    ClaudeCodeRuntime::new(state.path()).on_spawn(wt.path()).unwrap();
    CodexRuntime::new(state.path()).on_spawn(wt.path()).unwrap();

    for shim in [
        ClaudeCodeRuntime::new(state.path()).shim_path(),
        CodexRuntime::new(state.path()).shim_path(),
    ] {
        let out = std::process::Command::new("node").arg("--check").arg(&shim).output();
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
