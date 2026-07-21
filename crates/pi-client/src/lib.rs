//! pi-client — typed client for pi's RPC mode (architecture §4.2–4.3, M8).
//!
//! Phase 2's foundation: spawn `pi --mode rpc` as a headless sidecar, speak the
//! JSONL protocol, and fan the event stream out to whatever wants it (the
//! native chat tab, the sidebar, the notifier).
//!
//! Headless and UI-free like the other backend crates — the chat *panel* lives
//! in `harness-app`, but everything protocol-shaped lives here so it can be
//! conformance-tested against recorded sessions with no window.
//!
//! ```no_run
//! use pi_client::{PiRpc, RpcCommand};
//! let mut pi = PiRpc::spawn("/path/to/worktree", &["--model", "lmstudio/omar-agent-v1"]).unwrap();
//! let events = pi.subscribe();
//! pi.send(&RpcCommand::Prompt {
//!     id: Some("c1".into()),
//!     message: "fix the failing test".into(),
//!     streaming_behavior: None,
//! })
//! .unwrap();
//! for record in events {
//!     println!("{record:?}");
//! }
//! ```

pub mod framing;
pub mod protocol;
pub mod settle;

pub use protocol::{
    ExtensionUiRequest, ExtensionUiResponse, RpcCommand, RpcEvent, RpcRecord, RpcResponse,
    StreamingBehavior,
};
pub use settle::{AgentState, SettleTracker};

use std::io::{BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};

use crossbeam_channel::{unbounded, Receiver, Sender};

/// A running `pi --mode rpc` sidecar.
pub struct PiRpc {
    child: Child,
    stdin: std::process::ChildStdin,
    /// Fan-out subscribers for the record stream.
    subscribers: std::sync::Arc<std::sync::Mutex<Vec<Sender<RpcRecord>>>>,
}

impl PiRpc {
    /// Spawn `pi --mode rpc` with `cwd` set to the worktree root.
    ///
    /// `pi` is resolved from `$PATH` — the user's own installed, authenticated
    /// CLI, exactly as §4.5/R12 requires.
    pub fn spawn(worktree_root: impl AsRef<Path>, extra_args: &[&str]) -> std::io::Result<PiRpc> {
        let mut command = Command::new("pi");
        command
            .arg("--mode")
            .arg("rpc")
            .args(extra_args)
            .current_dir(worktree_root.as_ref())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        let mut child = command.spawn()?;
        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");

        let subscribers: std::sync::Arc<std::sync::Mutex<Vec<Sender<RpcRecord>>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));

        let pump_subs = std::sync::Arc::clone(&subscribers);
        std::thread::Builder::new()
            .name("pi-rpc-reader".into())
            .spawn(move || {
                let mut reader = BufReader::new(stdout);
                // Strict LF framing (§4.2) — see framing.rs for why a generic
                // line reader is not safe here.
                while let Ok(Some(record)) = framing::read_record(&mut reader) {
                    let parsed = match RpcRecord::parse(&record) {
                        Ok(parsed) => parsed,
                        // Unparseable JSON is a protocol violation, not a
                        // reason to tear down a session mid-run.
                        Err(_) => continue,
                    };
                    pump_subs
                        .lock()
                        .unwrap()
                        .retain(|tx| tx.send(parsed.clone()).is_ok());
                }
            })?;

        Ok(PiRpc {
            child,
            stdin,
            subscribers,
        })
    }

    /// Take a subscription to the record stream.
    ///
    /// Fan-out, like the `agent-runtime` bus: the chat tab, the sidebar, and
    /// the notifier all need the same records.
    pub fn subscribe(&self) -> Receiver<RpcRecord> {
        let (tx, rx) = unbounded();
        self.subscribers.lock().unwrap().push(tx);
        rx
    }

    /// Send a command.
    pub fn send(&mut self, command: &RpcCommand) -> std::io::Result<()> {
        let record = command
            .to_record()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        self.stdin.write_all(record.as_bytes())?;
        self.stdin.flush()
    }

    /// Answer a blocking [`ExtensionUiRequest`] (§4.3).
    pub fn reply(&mut self, response: &ExtensionUiResponse) -> std::io::Result<()> {
        let record = response
            .to_record()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        self.stdin.write_all(record.as_bytes())?;
        self.stdin.flush()
    }

    /// Terminate the sidecar.
    pub fn shutdown(&mut self) -> std::io::Result<()> {
        self.child.kill()?;
        self.child.wait().map(|_| ())
    }
}

impl Drop for PiRpc {
    fn drop(&mut self) {
        // A leaked pi process per closed worktree would be the M5 pane-cache
        // problem all over again, so never rely on the caller to do this.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
