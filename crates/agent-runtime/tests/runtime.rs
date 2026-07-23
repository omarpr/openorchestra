//! A1 tests — the adapter layer and the bus (architecture §4.5–4.6).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use agent_runtime::{
    AgentKind, AgentRuntime, AgentSignal, AgentState, Bus, PiRuntime, SignalKind,
    StatusFileSource, TerminalOnlyRuntime, WaitFidelity, WaitReason,
};

const RECV: Duration = Duration::from_secs(10);
const QUIET: Duration = Duration::from_millis(400);

// ---- bus fan-out -----------------------------------------------------------

fn signal(root: &str, state: AgentState) -> AgentSignal {
    AgentSignal {
        worktree_root: PathBuf::from(root),
        kind: AgentKind::Pi,
        state,
        reason: WaitReason::None,
        session_file: None,
    }
}

#[test]
fn bus_delivers_every_signal_to_every_subscriber() {
    // Fan-out, not work-stealing: the sidebar, status bar, and notifier each
    // need the same event. A plain multi-consumer channel would hand it to
    // exactly one of them.
    let bus = Bus::new();
    let a = bus.subscribe();
    let b = bus.subscribe();

    bus.publish(signal("/wt/one", AgentState::Running));

    assert_eq!(a.recv_timeout(RECV).unwrap().state, AgentState::Running);
    assert_eq!(b.recv_timeout(RECV).unwrap().state, AgentState::Running);
}

#[test]
fn bus_prunes_subscribers_whose_receiver_was_dropped() {
    let bus = Bus::new();
    let keep = bus.subscribe();
    drop(bus.subscribe());
    assert_eq!(bus.subscriber_count(), 2);

    bus.publish(signal("/wt/one", AgentState::Idle));

    assert_eq!(bus.subscriber_count(), 1, "dead subscriber must be dropped");
    assert_eq!(keep.recv_timeout(RECV).unwrap().state, AgentState::Idle);
}

// ---- notification gating (R11) ---------------------------------------------

#[test]
fn only_an_exact_signal_may_claim_input_is_needed() {
    // R11: a best-effort adapter reporting turn-complete must never be dressed
    // up as "input needed", or the notification becomes noise users disable.
    let waiting = signal("/wt/one", AgentState::Waiting);
    assert!(waiting.should_notify(WaitFidelity::Exact));
    assert!(!waiting.should_notify(WaitFidelity::BestEffort));
    assert!(!waiting.should_notify(WaitFidelity::TerminalOnly));

    for state in [AgentState::Running, AgentState::Idle, AgentState::Unknown] {
        assert!(
            !signal("/wt/one", state).should_notify(WaitFidelity::Exact),
            "{state:?} is not a request for input"
        );
    }
}

// ---- pi adapter ------------------------------------------------------------

#[test]
fn pi_materializes_the_embedded_extension_on_spawn() {
    // The regression this guards: before A1 nothing installed the extension at
    // all, so .harness/status.json was never written and the sidebar pill was
    // permanently Unknown in a real run.
    let state = tempfile::tempdir().unwrap();
    let worktree = tempfile::tempdir().unwrap();
    let pi = PiRuntime::new(state.path());

    assert!(!pi.extension_path().exists());
    pi.on_spawn(worktree.path()).unwrap();

    let written = fs::read_to_string(pi.extension_path()).unwrap();
    assert!(
        written.contains("harness-status"),
        "the embedded extension source should be written verbatim"
    );
    assert!(
        written.contains("status.json"),
        "extension must be the one that writes the status file"
    );
}

#[test]
fn pi_on_spawn_is_idempotent_and_leaves_no_temp_files() {
    let state = tempfile::tempdir().unwrap();
    let worktree = tempfile::tempdir().unwrap();
    let pi = PiRuntime::new(state.path());

    pi.on_spawn(worktree.path()).unwrap();
    let first = fs::read_to_string(pi.extension_path()).unwrap();
    pi.on_spawn(worktree.path()).unwrap();
    let second = fs::read_to_string(pi.extension_path()).unwrap();
    assert_eq!(first, second);

    // Only the materialized assets themselves — no `.tmp` residue.
    let expected = ["harness-status.ts", "harness-tools.ts", "tools.json"];
    let strays: Vec<_> = fs::read_dir(pi.asset_dir())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !expected.contains(&n.as_str()))
        .collect();
    assert!(strays.is_empty(), "atomic write left {strays:?} behind");
}

#[test]
fn pi_materializes_the_tool_contract_for_the_extension_to_read() {
    // M6: harness-tools.ts pins pi's toolset to the contract, so the contract
    // has to travel with it — a packaged .app has no repo to walk up from.
    let state = tempfile::tempdir().unwrap();
    let worktree = tempfile::tempdir().unwrap();
    let pi = PiRuntime::new(state.path());
    pi.on_spawn(worktree.path()).unwrap();

    let contract: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(pi.schema_path()).unwrap()).unwrap();
    assert_eq!(contract["schemaVersion"], "1.0");
    assert!(fs::read_to_string(pi.tools_extension_path())
        .unwrap()
        .contains("setActiveTools"));
}

#[test]
fn pi_command_points_the_extension_at_the_materialized_contract() {
    let state = tempfile::tempdir().unwrap();
    let worktree = tempfile::tempdir().unwrap();
    let pi = PiRuntime::new(state.path());
    let Some(cmd) = pi.command(worktree.path()) else {
        eprintln!("pi not on PATH — skipping");
        return;
    };
    // Both extensions loaded.
    let ext_args: Vec<&String> = cmd
        .args
        .iter()
        .zip(cmd.args.iter().skip(1))
        .filter(|(flag, _)| flag.as_str() == "-e")
        .map(|(_, path)| path)
        .collect();
    assert_eq!(ext_args.len(), 2, "both extensions should be passed: {:?}", cmd.args);

    let schema_env = cmd
        .env
        .iter()
        .find(|(k, _)| k == "HARNESS_AGENT_SCHEMA")
        .expect("HARNESS_AGENT_SCHEMA must be set so a packaged app can find the contract");
    assert_eq!(Path::new(&schema_env.1), pi.schema_path());
}

#[test]
fn pi_refreshes_a_stale_extension() {
    // An openOrchestra upgrade ships a new extension; the next spawn must pick
    // it up rather than keep running last release's copy.
    let state = tempfile::tempdir().unwrap();
    let worktree = tempfile::tempdir().unwrap();
    let pi = PiRuntime::new(state.path());
    pi.on_spawn(worktree.path()).unwrap();

    fs::write(pi.extension_path(), "// stale contents from an older build\n").unwrap();
    pi.on_spawn(worktree.path()).unwrap();

    let refreshed = fs::read_to_string(pi.extension_path()).unwrap();
    assert!(!refreshed.starts_with("// stale"), "stale copy was not replaced");
}

#[test]
fn pi_reports_its_signal_surface_honestly() {
    let state = tempfile::tempdir().unwrap();
    let pi = PiRuntime::new(state.path());
    assert_eq!(pi.kind(), AgentKind::Pi);
    assert_eq!(pi.signal_kind(), SignalKind::StatusFile);
    // Not Exact until Phase 2's RPC events retire the R8 ceiling.
    assert_eq!(pi.fidelity(), WaitFidelity::BestEffort);
    // Its signal arrives via the shared StatusFileSource, not its own stream.
    assert!(pi.subscribe().is_none());
}

#[test]
fn pi_command_loads_the_extension_explicitly() {
    let state = tempfile::tempdir().unwrap();
    let worktree = tempfile::tempdir().unwrap();
    let pi = PiRuntime::new(state.path());

    // Skipped rather than failed when pi is not installed: `command` returning
    // None is the documented "fall back to a plain shell" path.
    let Some(cmd) = pi.command(worktree.path()) else {
        eprintln!("pi not on PATH — skipping");
        return;
    };
    assert_eq!(cmd.label, "pi");
    assert_eq!(cmd.args[0], "-e");
    assert_eq!(Path::new(&cmd.args[1]), pi.extension_path());
    assert!(pi.extension_path().exists());
}

// ---- terminal-only baseline (R11) ------------------------------------------

#[test]
fn terminal_only_agents_are_a_supported_case_not_a_failure() {
    let rt = TerminalOnlyRuntime::new(AgentKind::Kimi);
    assert_eq!(rt.fidelity(), WaitFidelity::TerminalOnly);
    assert_eq!(rt.signal_kind(), SignalKind::None);
    assert!(rt.subscribe().is_none());
    // on_spawn must succeed — there is simply nothing to configure.
    assert!(rt.on_spawn(Path::new("/tmp")).is_ok());
}

#[test]
fn claude_code_maps_to_the_claude_binary() {
    // The registry kind is "claude-code" but the CLI on $PATH is `claude`;
    // getting this wrong means the terminal silently falls back to a shell.
    let rt = TerminalOnlyRuntime::new(AgentKind::ClaudeCode);
    if let Some(cmd) = rt.command(Path::new("/tmp")) {
        assert_eq!(cmd.label, "claude");
        assert!(cmd.program.ends_with("claude"));
    }
}

#[test]
fn settings_default_display_matches_what_the_spawn_actually_runs() {
    // The default command is defined once per agent (its module's PROGRAM /
    // DEFAULT_ARGS consts); the settings UI shows `default_command_display`
    // and the adapter spawns `command()`. This pins the two to each other so
    // a flag added to one place cannot silently miss the other. pi is absent
    // by design: its spawn appends `-e <extension>` wiring args that are not
    // part of the user-facing default.
    let dir = tempfile::tempdir().unwrap();
    let kinds: Vec<(AgentKind, Box<dyn AgentRuntime>)> = vec![
        (
            AgentKind::ClaudeCode,
            Box::new(agent_runtime::ClaudeCodeRuntime::new(dir.path())),
        ),
        (
            AgentKind::Codex,
            Box::new(agent_runtime::CodexRuntime::new(dir.path())),
        ),
        (AgentKind::Kimi, Box::new(agent_runtime::KimiRuntime)),
        (
            AgentKind::OpenCode,
            Box::new(agent_runtime::OpenCodeRuntime::new("/tmp/wt")),
        ),
    ];
    for (kind, rt) in kinds {
        let display = agent_runtime::default_command_display(&kind);
        let mut parts = display.split_whitespace();
        let program = parts.next().expect("display always names a binary");
        // Skipped rather than failed when the CLI is not installed.
        let Some(cmd) = rt.command(Path::new("/tmp")) else {
            continue;
        };
        assert!(cmd.program.ends_with(program), "{kind}: {:?}", cmd.program);
        assert_eq!(
            cmd.args,
            parts.map(String::from).collect::<Vec<_>>(),
            "{kind}: spawn args diverge from the settings default"
        );
    }
    // The flags themselves live in exactly one place; this is a change
    // detector, not a second definition.
    assert_eq!(
        agent_runtime::default_command_display(&AgentKind::ClaudeCode),
        "claude --permission-mode auto"
    );
    assert_eq!(
        agent_runtime::default_command_display(&AgentKind::Codex),
        "codex --ask-for-approval never --dangerously-bypass-hook-trust"
    );
}

#[test]
fn unknown_agent_kinds_still_get_a_runnable_adapter() {
    let rt = agent_runtime::runtime_for(
        &AgentKind::Other("future-agent".into()),
        Path::new("/tmp/worktree"),
        Path::new("/tmp/state"),
    );
    assert_eq!(rt.fidelity(), WaitFidelity::TerminalOnly);
    assert_eq!(rt.kind(), AgentKind::Other("future-agent".into()));
}

// ---- status-file source ----------------------------------------------------

fn write_status(root: &Path, state: &str) {
    let dir = root.join(".harness");
    fs::create_dir_all(&dir).unwrap();
    let tmp = dir.join(".status.tmp");
    fs::write(
        &tmp,
        format!(r#"{{"state":"{state}","timestamp":"2026-07-21T00:00:00Z","sessionFile":"/s.jsonl"}}"#),
    )
    .unwrap();
    fs::rename(&tmp, dir.join("status.json")).unwrap();
}

#[test]
fn status_file_writes_reach_the_bus_as_normalized_signals() {
    let bus = Bus::new();
    let rx = bus.subscribe();
    let mut source = StatusFileSource::new(bus).unwrap();
    let wt = tempfile::tempdir().unwrap();

    source.watch(wt.path(), AgentKind::Pi).unwrap();
    // Initial Unknown (no file yet).
    assert_eq!(rx.recv_timeout(RECV).unwrap().state, AgentState::Unknown);

    write_status(wt.path(), "running");
    let running = rx.recv_timeout(RECV).unwrap();
    assert_eq!(running.state, AgentState::Running);
    assert_eq!(running.kind, AgentKind::Pi);
    assert_eq!(running.reason, WaitReason::None);
    assert_eq!(running.session_file, Some(PathBuf::from("/s.jsonl")));

    write_status(wt.path(), "waiting");
    let waiting = rx.recv_timeout(RECV).unwrap();
    assert_eq!(waiting.state, AgentState::Waiting);
    assert_eq!(waiting.reason, WaitReason::PermissionPrompt);

    write_status(wt.path(), "idle");
    let idle = rx.recv_timeout(RECV).unwrap();
    assert_eq!(idle.state, AgentState::Idle);
    assert_eq!(idle.reason, WaitReason::TurnComplete);
}

#[test]
fn status_file_source_attributes_signals_to_the_right_agent() {
    // pi and Claude Code share the status file format, so the source — not the
    // file — has to say which agent produced it.
    let bus = Bus::new();
    let rx = bus.subscribe();
    let mut source = StatusFileSource::new(bus).unwrap();
    let wt = tempfile::tempdir().unwrap();

    source.watch(wt.path(), AgentKind::ClaudeCode).unwrap();
    rx.recv_timeout(RECV).unwrap(); // initial

    write_status(wt.path(), "waiting");
    let sig = rx.recv_timeout(RECV).unwrap();
    assert_eq!(sig.kind, AgentKind::ClaudeCode);
    assert_eq!(sig.state, AgentState::Waiting);
}

#[test]
fn unwatching_stops_signals_for_that_worktree() {
    let bus = Bus::new();
    let rx = bus.subscribe();
    let mut source = StatusFileSource::new(bus).unwrap();
    let wt = tempfile::tempdir().unwrap();

    source.watch(wt.path(), AgentKind::Pi).unwrap();
    rx.recv_timeout(RECV).unwrap();

    source.unwatch(wt.path()).unwrap();
    write_status(wt.path(), "running");
    assert!(
        rx.recv_timeout(QUIET).is_err(),
        "no signals after unwatch — a closed worktree must not keep pinging"
    );
}
