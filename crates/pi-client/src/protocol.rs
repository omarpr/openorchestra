//! Serde types mirroring pi's `rpc-types.ts` (architecture §4.2).
//!
//! Verified against **pi 0.73.1** by reading `dist/modes/rpc/rpc-types.d.ts`
//! and by recording a real `pi --mode rpc` session
//! (`fixtures/rpc-session-0.73.1.jsonl`).
//!
//! ## Two corrections to §4.2's sketch, found by checking rather than assuming
//!
//! 1. **`agent_settled` does not exist.** §4.2 calls it "the true done signal"
//!    and R8 expects it to retire the Phase 1 debounce. It appears nowhere in
//!    0.73.1's RPC mode or its docs. The real event list ends at `agent_end`.
//!    What *does* exist, and what actually fixes R8's first gap, is
//!    `auto_retry_start` / `auto_retry_end`: the extension path had to guess at
//!    retries because `agent_end`/`agent_start` re-fire with no flag, whereas
//!    RPC says so explicitly. See [`SettleTracker`].
//! 2. **`get_entries` / `get_tree` do not exist.** §4.2 calls `get_entries`
//!    "the durable resume primitive across harness restarts". 0.73.1 offers
//!    `get_messages`, `get_fork_messages`, and `get_last_assistant_text`
//!    instead — none of which take a `since` cursor, so resume means replaying
//!    the session file, not a cursor query.
//!
//! Unknown records are preserved rather than rejected ([`RpcRecord::Unknown`]),
//! so a pi upgrade that adds an event degrades to "ignored" instead of
//! "client dies mid-session".

use serde::{Deserialize, Serialize};

/// A command sent to pi on stdin.
///
/// Only the subset the harness actually drives is modelled; `Raw` is the
/// escape hatch for the rest rather than 25 near-identical variants.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RpcCommand {
    Prompt {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none", rename = "streamingBehavior")]
        streaming_behavior: Option<StreamingBehavior>,
    },
    Steer {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        message: String,
    },
    FollowUp {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        message: String,
    },
    Abort {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    GetState {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    GetMessages {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    GetSessionStats {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    NewSession {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    SwitchSession {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(rename = "sessionPath")]
        session_path: String,
    },
    SetModel {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        provider: String,
        #[serde(rename = "modelId")]
        model_id: String,
    },
    Compact {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    SetAutoRetry {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        enabled: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StreamingBehavior {
    Steer,
    FollowUp,
}

impl RpcCommand {
    /// Serialize as one framed record, terminator included.
    pub fn to_record(&self) -> Result<String, serde_json::Error> {
        Ok(format!("{}\n", serde_json::to_string(self)?))
    }
}

/// Anything pi writes on stdout.
#[derive(Debug, Clone, PartialEq)]
pub enum RpcRecord {
    Event(RpcEvent),
    Response(RpcResponse),
    ExtensionUiRequest(ExtensionUiRequest),
    /// A record this build does not model. Kept verbatim so a pi upgrade is a
    /// no-op rather than a crash.
    Unknown(serde_json::Value),
}

impl RpcRecord {
    /// Classify one framed record.
    pub fn parse(record: &str) -> Result<RpcRecord, serde_json::Error> {
        let value: serde_json::Value = serde_json::from_str(record)?;
        Ok(Self::from_value(value))
    }

    pub fn from_value(value: serde_json::Value) -> RpcRecord {
        match value.get("type").and_then(|t| t.as_str()) {
            Some("response") => serde_json::from_value(value.clone())
                .map(RpcRecord::Response)
                .unwrap_or(RpcRecord::Unknown(value)),
            Some("extension_ui_request") => serde_json::from_value(value.clone())
                .map(RpcRecord::ExtensionUiRequest)
                .unwrap_or(RpcRecord::Unknown(value)),
            Some(_) => serde_json::from_value(value.clone())
                .map(RpcRecord::Event)
                .unwrap_or(RpcRecord::Unknown(value)),
            None => RpcRecord::Unknown(value),
        }
    }
}

/// The event stream (§4.2), as it actually exists at 0.73.1.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RpcEvent {
    AgentStart,
    AgentEnd {
        #[serde(default)]
        messages: Vec<serde_json::Value>,
    },
    TurnStart,
    TurnEnd {
        #[serde(default)]
        message: serde_json::Value,
        #[serde(default, rename = "toolResults")]
        tool_results: Vec<serde_json::Value>,
    },
    MessageStart {
        message: serde_json::Value,
    },
    /// Streaming deltas. Note the field is `assistantMessageEvent`, not the
    /// `delta` §4.2 sketched — the inner `type` is `text_delta`,
    /// `thinking_delta`, `toolcall_start`, `toolcall_delta`, ...
    MessageUpdate {
        #[serde(rename = "assistantMessageEvent")]
        assistant_message_event: serde_json::Value,
        #[serde(default)]
        message: serde_json::Value,
    },
    MessageEnd {
        message: serde_json::Value,
    },
    ToolExecutionStart {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "toolName")]
        tool_name: String,
        #[serde(default)]
        args: serde_json::Value,
    },
    ToolExecutionUpdate {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "toolName")]
        tool_name: String,
        #[serde(default, rename = "partialResult")]
        partial_result: serde_json::Value,
    },
    ToolExecutionEnd {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "toolName")]
        tool_name: String,
        #[serde(default)]
        result: serde_json::Value,
        #[serde(default, rename = "isError")]
        is_error: bool,
    },
    QueueUpdate {
        #[serde(default)]
        queue: serde_json::Value,
    },
    CompactionStart,
    CompactionEnd {
        #[serde(default)]
        result: serde_json::Value,
    },
    /// The event that actually retires R8's retry gap: the Phase 1 extension
    /// had to infer retries from a bare `agent_end`/`agent_start` pair.
    AutoRetryStart {
        #[serde(default)]
        attempt: Option<u32>,
        #[serde(default)]
        error: Option<String>,
    },
    AutoRetryEnd {
        #[serde(default)]
        succeeded: Option<bool>,
    },
    ExtensionError {
        #[serde(default, rename = "extensionPath")]
        extension_path: Option<String>,
        #[serde(default)]
        error: Option<String>,
    },
}

/// A command acknowledgement, correlated by `id`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct RpcResponse {
    #[serde(default)]
    pub id: Option<String>,
    pub command: String,
    pub success: bool,
    #[serde(default)]
    pub data: Option<serde_json::Value>,
    #[serde(default)]
    pub error: Option<String>,
}

/// A blocking dialog request from an extension (§4.3).
///
/// These are what retire R8's *second* gap: in Phase 1 the harness could only
/// see dialogs opened through the shared extension UI object, whereas here pi
/// tells us directly, with an id to answer on.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ExtensionUiRequest {
    pub id: String,
    pub method: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub options: Option<Vec<String>>,
    #[serde(default)]
    pub placeholder: Option<String>,
    #[serde(default)]
    pub prefill: Option<String>,
}

impl ExtensionUiRequest {
    /// Whether this request blocks the agent until answered.
    ///
    /// Fire-and-forget methods (`notify`, `setStatus`, `setWidget`,
    /// `setTitle`, `set_editor_text`) map to toasts and status entries and must
    /// NOT raise a modal or a "needs input" notification — treating them as
    /// blocking would spam the user with dialogs the agent never waited on.
    pub fn is_blocking(&self) -> bool {
        matches!(
            self.method.as_str(),
            "select" | "confirm" | "input" | "editor"
        )
    }
}

/// A reply to a blocking [`ExtensionUiRequest`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExtensionUiResponse {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirmed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancelled: Option<bool>,
}

impl ExtensionUiResponse {
    pub fn value(id: impl Into<String>, value: impl Into<String>) -> Self {
        ExtensionUiResponse {
            kind: "extension_ui_response",
            id: id.into(),
            value: Some(value.into()),
            confirmed: None,
            cancelled: None,
        }
    }

    pub fn confirmed(id: impl Into<String>, confirmed: bool) -> Self {
        ExtensionUiResponse {
            kind: "extension_ui_response",
            id: id.into(),
            value: None,
            confirmed: Some(confirmed),
            cancelled: None,
        }
    }

    pub fn cancelled(id: impl Into<String>) -> Self {
        ExtensionUiResponse {
            kind: "extension_ui_response",
            id: id.into(),
            value: None,
            confirmed: None,
            cancelled: Some(true),
        }
    }

    pub fn to_record(&self) -> Result<String, serde_json::Error> {
        Ok(format!("{}\n", serde_json::to_string(self)?))
    }
}
