//! Live integration against a real `pi --mode rpc` process.
//!
//! Ignored by default because it needs `pi` and `node` installed and binds a
//! local port. Run it after any pi bump — it is the RPC-side counterpart of
//! `pi-ext/testing/capture-lifecycle.sh`:
//!
//! ```sh
//! cargo test -p pi-client --test live -- --ignored --nocapture
//! ```
//!
//! Conformance tests prove we parse a *recording*; this proves we can still
//! drive the real thing — spawn, frame, correlate, and settle.

use std::io::Write;
use std::net::TcpListener;
use std::time::Duration;

use pi_client::{AgentState, PiRpc, RpcCommand, RpcRecord, SettleTracker};

/// The same OpenAI-compatible stub the pi-ext harness uses, inline so this test
/// needs no external process management.
fn spawn_stub_provider(port: u16) -> std::process::Child {
    let script = format!(
        r#"
const http = require("node:http");
let turn = 0;
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
    turn++;
    setTimeout(() => {{
      sse(res, {{...base, choices: [{{index: 0, delta: {{role: "assistant", content: "pong"}}, finish_reason: null}}]}});
      sse(res, {{...base, choices: [{{index: 0, delta: {{}}, finish_reason: "stop"}}]}});
      res.write("data: [DONE]\n\n");
      res.end();
    }}, 300);
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
        .expect("node is required for the live test")
}

#[test]
#[ignore = "needs pi + node; run after a pi version bump"]
fn drives_a_real_pi_rpc_process() {
    let port = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let mut stub = spawn_stub_provider(port);
    std::thread::sleep(Duration::from_millis(700));

    let work = tempfile::tempdir().unwrap();
    let agent_dir = tempfile::tempdir().unwrap();
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
    // Sandbox pi so the user's own ~/.pi is never read or written.
    std::env::set_var("PI_CODING_AGENT_DIR", agent_dir.path());

    let mut pi = PiRpc::spawn("pi", work.path(), &["--model", "lmstudio/omar-agent-v1"])
        .expect("pi must be on PATH");
    let records = pi.subscribe();

    pi.send(&RpcCommand::GetState { id: Some("s1".into()) }).unwrap();
    pi.send(&RpcCommand::Prompt {
        id: Some("p1".into()),
        message: "ping".into(),
        streaming_behavior: None,
    })
    .unwrap();

    let mut tracker = SettleTracker::new();
    let mut saw_state_response = false;
    let mut transitions = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(45);

    while std::time::Instant::now() < deadline {
        let Ok(record) = records.recv_timeout(Duration::from_secs(5)) else {
            break;
        };
        if let RpcRecord::Response(resp) = &record {
            if resp.id.as_deref() == Some("s1") {
                assert_eq!(resp.command, "get_state");
                assert!(resp.success, "get_state failed: {:?}", resp.error);
                saw_state_response = true;
            }
        }
        if let Some(state) = tracker.observe(&record) {
            transitions.push(state);
            if state == AgentState::Idle {
                break;
            }
        }
    }

    let _ = pi.shutdown();
    let _ = stub.kill();
    let _ = stub.wait();
    let _ = std::io::stdout().flush();

    assert!(saw_state_response, "get_state response never correlated by id");
    assert_eq!(
        transitions,
        vec![AgentState::Running, AgentState::Idle],
        "a real prompt must run then settle"
    );
}
