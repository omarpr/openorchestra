//! Phase 2 chat pane — the native agent surface over pi's RPC mode
//! (architecture §4.1–4.3, roadmap M8).
//!
//! v1's agent surface is pi's own TUI in a terminal, which costs no UI work but
//! gives us nothing structured to render. This tab drives a headless
//! `pi --mode rpc` sidecar instead, so the conversation is real data: streaming
//! text, tool calls with their arguments and results, and — crucially —
//! `extension_ui_request` dialogs the harness can answer natively (§4.3).
//!
//! All protocol handling lives in `crates/pi-client`, which is headless and
//! conformance-tested against recorded pi output. This file is only the view.
//!
//! The terminal tab remains the default and the fallback: this pane is additive,
//! and a worktree whose `pi` is missing simply shows why rather than breaking.

use std::path::PathBuf;

use agent_runtime::AgentKind;

use gpui::{
    App, Context, FocusHandle, Focusable, IntoElement, KeyDownEvent, ParentElement, Render, Styled,
    Window, div, prelude::*, px,
};

use pi_client::{
    AgentState, ExtensionUiRequest, ExtensionUiResponse, PiRpc, RpcCommand, RpcEvent, RpcRecord,
    SettleTracker,
};

use crate::theme::{HarnessTheme, theme};

/// One rendered line of the conversation.
#[derive(Debug, Clone)]
enum Entry {
    User(String),
    /// Assistant text. Streaming deltas append to the last one rather than
    /// creating a new entry per token.
    Assistant(String),
    ToolCall {
        name: String,
        args: String,
    },
    ToolResult {
        name: String,
        text: String,
        is_error: bool,
    },
    Notice(String),
}

pub struct ChatPane {
    rpc: Option<PiRpc>,
    entries: Vec<Entry>,
    /// Set while the assistant is mid-message, so text deltas append.
    streaming: bool,
    state: AgentState,
    tracker: SettleTracker,
    /// The dialog currently blocking the agent, if any (§4.3).
    pending_dialog: Option<ExtensionUiRequest>,
    input: String,
    error: Option<String>,
    worktree_root: PathBuf,
    focus_handle: FocusHandle,
}

impl ChatPane {
    pub fn new(worktree_root: PathBuf, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            rpc: None,
            entries: Vec::new(),
            streaming: false,
            state: AgentState::Idle,
            tracker: SettleTracker::new(),
            pending_dialog: None,
            input: String::new(),
            error: None,
            worktree_root,
            focus_handle: cx.focus_handle(),
        };
        this.connect(cx);
        this
    }

    fn connect(&mut self, cx: &mut Context<Self>) {
        // The binary name comes from pi's one definition in agent-runtime;
        // `--mode rpc` is pi-client's own contract, not a default-args thing.
        let (pi_bin, _) = agent_runtime::default_invocation(&AgentKind::Pi);
        // Spawned without a shell: on Windows npm's `pi` is an sh wrapper and
        // a bare name finds only `pi.exe`, so start the `.cmd` beside it.
        let program = agent_runtime::resolve_on_path(pi_bin)
            .map(|path| os_compat::Os::CURRENT.direct_launcher(&path).into_os_string())
            .unwrap_or_else(|| pi_bin.into());
        match PiRpc::spawn(program, &self.worktree_root, &[]) {
            Ok(rpc) => {
                let records = rpc.subscribe();
                self.rpc = Some(rpc);
                self.entries
                    .push(Entry::Notice("pi session started".into()));

                // Poll, never block. `cx.spawn` runs on the FOREGROUND
                // executor, so a blocking `recv()` here wedges the UI thread
                // for as long as pi has nothing to say — which is most of the
                // time, and looked exactly like the app hanging on tab click.
                // Same try_recv + timer shape the forge and status channels use.
                cx.spawn(async move |view, cx| {
                    loop {
                        match records.try_recv() {
                            Ok(record) => {
                                if view.update(cx, |pane, cx| pane.apply(record, cx)).is_err() {
                                    break; // pane went away
                                }
                            }
                            Err(e) if e.is_empty() => {
                                cx.background_executor()
                                    .timer(std::time::Duration::from_millis(60))
                                    .await
                            }
                            // Sidecar exited and closed the channel.
                            Err(_) => break,
                        }
                    }
                })
                .detach();
            }
            Err(e) => {
                // Not fatal: the terminal tab is still the working agent
                // surface, so say what happened rather than showing nothing.
                self.error = Some(format!(
                    "could not start `pi --mode rpc`: {e}. The ❯_ terminal tab still works."
                ));
            }
        }
    }

    fn apply(&mut self, record: RpcRecord, cx: &mut Context<Self>) {
        if let Some(state) = self.tracker.observe(&record) {
            self.state = state;
        }
        match record {
            RpcRecord::Event(RpcEvent::MessageUpdate {
                assistant_message_event,
                ..
            }) => {
                // Only text deltas append; thinking and tool-call deltas are
                // represented by their own entries below.
                if assistant_message_event["type"] == "text_delta"
                    && let Some(delta) = assistant_message_event["delta"].as_str()
                {
                    self.append_assistant(delta);
                }
            }
            RpcRecord::Event(RpcEvent::ToolExecutionStart {
                tool_name, args, ..
            }) => {
                self.streaming = false;
                self.entries.push(Entry::ToolCall {
                    name: tool_name,
                    args: compact_json(&args),
                });
            }
            RpcRecord::Event(RpcEvent::ToolExecutionEnd {
                tool_name,
                result,
                is_error,
                ..
            }) => {
                self.entries.push(Entry::ToolResult {
                    name: tool_name,
                    text: text_of(&result),
                    is_error,
                });
            }
            RpcRecord::Event(RpcEvent::AgentEnd { .. }) => self.streaming = false,
            RpcRecord::ExtensionUiRequest(req) => {
                if req.is_blocking() {
                    // Blocking dialogs become a native prompt; fire-and-forget
                    // methods (notify/setStatus/...) must not, or the user gets
                    // modals the agent never waited on (§4.3).
                    self.pending_dialog = Some(req);
                } else if req.method == "notify"
                    && let Some(message) = req.message.clone()
                {
                    self.entries.push(Entry::Notice(message));
                }
            }
            RpcRecord::Response(resp) if !resp.success => {
                self.entries.push(Entry::Notice(format!(
                    "{} failed: {}",
                    resp.command,
                    resp.error.unwrap_or_default()
                )));
            }
            _ => {}
        }
        cx.notify();
    }

    fn append_assistant(&mut self, delta: &str) {
        if self.streaming
            && let Some(Entry::Assistant(text)) = self.entries.last_mut()
        {
            text.push_str(delta);
            return;
        }
        self.streaming = true;
        self.entries.push(Entry::Assistant(delta.to_string()));
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let message = std::mem::take(&mut self.input);
        let message = message.trim().to_string();
        if message.is_empty() {
            return;
        }
        let Some(rpc) = self.rpc.as_mut() else { return };

        // Steering mid-turn is a different command from a fresh prompt; sending
        // `prompt` while streaming would queue rather than interrupt.
        let command = if self.state == AgentState::Running {
            RpcCommand::Steer {
                id: None,
                message: message.clone(),
            }
        } else {
            RpcCommand::Prompt {
                id: None,
                message: message.clone(),
                streaming_behavior: None,
            }
        };
        if let Err(e) = rpc.send(&command) {
            self.entries
                .push(Entry::Notice(format!("send failed: {e}")));
        } else {
            self.entries.push(Entry::User(message));
        }
        cx.notify();
    }

    fn answer_dialog(&mut self, response: ExtensionUiResponse, cx: &mut Context<Self>) {
        if let Some(rpc) = self.rpc.as_mut() {
            let _ = rpc.reply(&response);
        }
        self.pending_dialog = None;
        if let Some(state) = self.tracker.dialog_answered() {
            self.state = state;
        }
        cx.notify();
    }

    fn key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "enter" => self.submit(cx),
            "backspace" => {
                self.input.pop();
                cx.notify();
            }
            "escape" => {
                // Escape aborts the turn — the same affordance pi's TUI gives.
                if let Some(rpc) = self.rpc.as_mut() {
                    let _ = rpc.send(&RpcCommand::Abort { id: None });
                }
                cx.notify();
            }
            _ => {
                let m = &event.keystroke.modifiers;
                if let Some(ch) = event.keystroke.key_char.clone()
                    && !ch.is_empty()
                    && !m.control
                    && !m.platform
                {
                    self.input.push_str(&ch);
                    cx.notify();
                }
            }
        }
        cx.stop_propagation();
    }

    fn render_entry(&self, hx: &HarnessTheme, entry: &Entry) -> impl IntoElement {
        let (label, body, color) = match entry {
            Entry::User(text) => ("you", text.clone(), hx.accent),
            Entry::Assistant(text) => ("pi", text.clone(), hx.tx0),
            Entry::ToolCall { name, args } => ("tool", format!("{name} {args}"), hx.accent),
            Entry::ToolResult {
                name,
                text,
                is_error,
            } => (
                if *is_error { "error" } else { "result" },
                format!("{name}: {text}"),
                if *is_error { hx.red } else { hx.tx2 },
            ),
            Entry::Notice(text) => ("·", text.clone(), hx.tx2),
        };
        div()
            .flex()
            .gap_2()
            .py_0p5()
            .px_3()
            .child(
                div()
                    .flex_none()
                    .w(px(52.0))
                    .text_xs()
                    .font_family(hx.font_mono)
                    .text_color(color)
                    .child(label),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .text_color(hx.tx1)
                    .child(truncate(&body, 4000)),
            )
    }
}

impl Focusable for ChatPane {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ChatPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let hx = theme(cx).clone();

        if let Some(error) = &self.error {
            return div()
                .size_full()
                .bg(hx.bg0)
                .p_3()
                .child(div().text_xs().text_color(hx.red).child(error.clone()));
        }

        let mut transcript = div().flex().flex_col().flex_1().min_h_0().overflow_hidden();
        for entry in &self.entries {
            transcript = transcript.child(self.render_entry(&hx, entry));
        }

        let status = match self.state {
            AgentState::Running => ("running", hx.green),
            AgentState::Waiting => ("waiting on you", hx.amber),
            AgentState::Idle => ("idle", hx.tx2),
        };

        let mut root = div()
            .size_full()
            .flex()
            .flex_col()
            .bg(hx.bg0)
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::key))
            .child(transcript);

        // A blocking dialog takes precedence over the input: the agent is
        // waiting on this specific answer, not on a new prompt (§4.3).
        if let Some(dialog) = self.pending_dialog.clone() {
            let title = dialog.title.clone().unwrap_or_default();
            let message = dialog.message.clone().unwrap_or_default();
            let id_yes = dialog.id.clone();
            let id_no = dialog.id.clone();
            root = root.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .flex_none()
                    .p_3()
                    .bg(hx.bg1)
                    .border_t_1()
                    .border_color(hx.amber)
                    .child(div().text_xs().text_color(hx.tx0).child(title))
                    .child(div().text_xs().text_color(hx.tx2).child(message))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                div()
                                    .id("dialog-yes")
                                    .px_2()
                                    .py_0p5()
                                    .rounded_md()
                                    .bg(hx.bg0)
                                    .text_xs()
                                    .text_color(hx.green)
                                    .cursor_pointer()
                                    .child("approve")
                                    .on_click(cx.listener(move |pane, _, _, cx| {
                                        pane.answer_dialog(
                                            ExtensionUiResponse::confirmed(id_yes.clone(), true),
                                            cx,
                                        );
                                    })),
                            )
                            .child(
                                div()
                                    .id("dialog-no")
                                    .px_2()
                                    .py_0p5()
                                    .rounded_md()
                                    .bg(hx.bg0)
                                    .text_xs()
                                    .text_color(hx.red)
                                    .cursor_pointer()
                                    .child("deny")
                                    .on_click(cx.listener(move |pane, _, _, cx| {
                                        pane.answer_dialog(
                                            ExtensionUiResponse::confirmed(id_no.clone(), false),
                                            cx,
                                        );
                                    })),
                            ),
                    ),
            );
        } else {
            root = root.child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_2()
                    .h(px(30.0))
                    .px_3()
                    .bg(hx.bg1)
                    .border_t_1()
                    .border_color(hx.border)
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .font_family(hx.font_mono)
                            .text_color(status.1)
                            .child(status.0),
                    )
                    .child(div().flex_1().min_w_0().text_xs().text_color(hx.tx0).child(
                        if self.input.is_empty() {
                            "type a message · enter to send · esc aborts".to_string()
                        } else {
                            self.input.clone()
                        },
                    )),
            );
        }
        root
    }
}

/// Compact one-line rendering of tool arguments.
fn compact_json(value: &serde_json::Value) -> String {
    truncate(&serde_json::to_string(value).unwrap_or_default(), 200)
}

/// Pull the text parts out of a tool result's content array.
fn text_of(result: &serde_json::Value) -> String {
    let text = result["content"]
        .as_array()
        .map(|parts| {
            parts
                .iter()
                .filter_map(|p| p["text"].as_str())
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();
    truncate(&text, 500)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    // Char-wise, not byte-wise: tool output is frequently non-ASCII and
    // slicing bytes would panic mid-codepoint.
    s.chars().take(max).collect::<String>() + " …"
}
