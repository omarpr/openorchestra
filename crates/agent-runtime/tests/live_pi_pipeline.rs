//! End-to-end proof of the pi status pipeline (architecture §4.1, §4.5).
//!
//! This is the test for the bug A1 found: §4.1 claims the harness installs
//! `harness-status.ts` so the sidebar pill works, but nothing in the app ever
//! did — so `.harness/status.json` was never written in a real run and the pill
//! could only ever read `Unknown`. Unit tests cover each piece; this asserts
//! the pieces actually compose, driving a **real** pi process exactly the way
//! `ensure_pane` does:
//!
//! ```text
//!   runtime_for(agent_kind) -> on_spawn() -> command() -> spawn
//!            -> pi writes .harness/status.json
//!            -> StatusFileSource -> Bus -> AgentSignal
//! ```
//!
//! Ignored by default (needs `pi` and `node`, binds a port). Run after a pi
//! bump alongside `pi-ext/testing/capture-lifecycle.sh`:
//!
//! ```sh
//! cargo test -p agent-runtime --test live_pi_pipeline -- --ignored --nocapture
//! ```

use std::net::TcpListener;
use std::time::Duration;

use agent_runtime::{runtime_for, AgentKind, AgentState, Bus, StatusFileSource};

fn spawn_stub_provider(port: u16) -> std::process::Child {
    let script = format!(
        r#"
const http = require("node:http");
const sse = (res, o) => res.write("data: " + JSON.stringify(o) + "\n\n");
http.createServer((req, res) => {{
  if (req.url === "/v1/models") {{
    res.writeHead(200, {{"content-type": "application/json"}});
    return res.end(JSON.stringify({{object: "list", data: [{{id: "omar-agent-v1"}}]}}));
  }}
  let body = "";
  req.on("data", c => body += c);
  req.on("end", () => {{
    res.writeHead(200, {{"content-type": "text/event-stream"}});
    const base = {{id: "c", object: "chat.completion.chunk", created: 0, model: "omar-agent-v1"}};
    // Delay so `running` is observable before the turn ends.
    setTimeout(() => {{
      sse(res, {{...base, choices: [{{index: 0, delta: {{role: "assistant", content: "ok"}}, finish_reason: null}}]}});
      sse(res, {{...base, choices: [{{index: 0, delta: {{}}, finish_reason: "stop"}}]}});
      res.write("data: [DONE]\n\n");
      res.end();
    }}, 1200);
  }});
}}).listen({port}, "127.0.0.1");
"#
    );
    std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("node is required")
}

#[test]
#[ignore = "needs pi + node; run after a pi version bump"]
fn pi_spawned_through_the_adapter_drives_the_sidebar_pill() {
    let port = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let mut stub = spawn_stub_provider(port);
    std::thread::sleep(Duration::from_millis(700));

    let state_dir = tempfile::tempdir().unwrap();
    let worktree = tempfile::tempdir().unwrap();
    let agent_dir = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    std::fs::write(
        agent_dir.path().join("models.json"),
        format!(
            r#"{{"providers":{{"lmstudio":{{"baseUrl":"http://localhost:{port}/v1",
               "api":"openai-completions","apiKey":"lmstudio",
               "models":[{{"id":"omar-agent-v1","name":"Omar Agent v1","reasoning":false,
               "contextWindow":131072,"maxTokens":16384,
               "cost":{{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}}}}]}}}}}}"#
        ),
    )
    .unwrap();

    // --- exactly what ensure_pane does -------------------------------------
    let runtime = runtime_for(&AgentKind::Pi, worktree.path(), state_dir.path());
    runtime.on_spawn(worktree.path()).expect("on_spawn must succeed");
    let command = runtime
        .command(worktree.path())
        .expect("pi must be on PATH for this test");

    // --- and what the workspace does with the signal ------------------------
    let bus = Bus::new();
    let signals = bus.subscribe();
    let mut source = StatusFileSource::new(bus).unwrap();
    source.watch(worktree.path(), AgentKind::Pi).unwrap();
    assert_eq!(
        signals.recv_timeout(Duration::from_secs(5)).unwrap().state,
        AgentState::Unknown,
        "initial read before pi starts"
    );

    let mut pi = std::process::Command::new(&command.program)
        .args(&command.args)
        .args(["-p", "say ok", "--model", "lmstudio/omar-agent-v1"])
        .arg("--session-dir")
        .arg(sessions.path())
        .envs(command.env.iter().cloned())
        .env("PI_CODING_AGENT_DIR", agent_dir.path())
        .current_dir(worktree.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawning pi");

    // --- assert the pill would actually light up ----------------------------
    let mut states = Vec::new();
    let mut session_file = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while std::time::Instant::now() < deadline {
        let Ok(signal) = signals.recv_timeout(Duration::from_secs(10)) else {
            break;
        };
        assert_eq!(signal.kind, AgentKind::Pi, "signals must be attributed");
        assert_eq!(signal.worktree_root, worktree.path());
        if signal.session_file.is_some() {
            session_file = signal.session_file.clone();
        }
        if states.last() != Some(&signal.state) {
            states.push(signal.state);
        }
        if signal.state == AgentState::Idle && !states.is_empty() {
            break;
        }
    }

    let _ = pi.wait();
    let _ = stub.kill();
    let _ = stub.wait();

    // The extension really was installed and really was loaded.
    assert!(
        worktree.path().join(".harness/status.json").exists(),
        "pi never wrote a status file — the extension did not load"
    );
    assert!(
        states.contains(&AgentState::Running),
        "never observed Running; states were {states:?}"
    );
    assert_eq!(
        states.last(),
        Some(&AgentState::Idle),
        "agent must settle to Idle; states were {states:?}"
    );
    // The harvesting hook M7 depends on.
    let session_file = session_file.expect("no sessionFile ever reported");
    assert!(session_file.exists(), "{} is missing", session_file.display());
}
