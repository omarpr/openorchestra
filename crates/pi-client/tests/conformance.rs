//! M8 conformance tests — replay a recorded `pi --mode rpc` session
//! (architecture §4.2).
//!
//! `fixtures/rpc-session-0.73.1.jsonl` is a sanitized recording of a real pi
//! 0.73.1 RPC run (get_state, a prompt that calls a tool, get_session_stats).
//! Local paths and model labels are replaced with generic fixture values.

use std::io::BufReader;

use pi_client::framing::{read_record, split_records, strip_terminator};
use pi_client::protocol::ExtensionUiRequest;
use pi_client::{AgentState, ExtensionUiResponse, RpcCommand, RpcEvent, RpcRecord, SettleTracker};

const RECORDED: &str = include_str!("../fixtures/rpc-session-0.73.1.jsonl");

// ---- framing (§4.2's explicit warning) -------------------------------------

#[test]
fn records_split_on_lf_only_never_on_unicode_separators() {
    // The documented hazard: U+2028 / U+2029 are legal INSIDE a JSON string,
    // so a reader that treats them as newlines silently splits one record into
    // two unparseable halves. This is why the crate does not use a "nicer"
    // generic line reader.
    let payload = "{\"type\":\"message_update\",\"text\":\"a\u{2028}b\u{2029}c\"}";
    let stream = format!("{payload}\n");
    let records = split_records(&stream);
    assert_eq!(records.len(), 1, "U+2028/U+2029 must not split a record");
    assert!(RpcRecord::parse(records[0]).is_ok());
}

#[test]
fn a_trailing_cr_is_stripped_but_inner_whitespace_is_untouched() {
    assert_eq!(strip_terminator("{\"a\":1}\r\n"), "{\"a\":1}");
    assert_eq!(strip_terminator("{\"a\":1}\n"), "{\"a\":1}");
    assert_eq!(strip_terminator("{\"a\":1}"), "{\"a\":1}");
    // trim_end() would eat these; strip_terminator must not.
    assert_eq!(
        strip_terminator("{\"a\":\"x\u{2028}\"}\n"),
        "{\"a\":\"x\u{2028}\"}"
    );
}

#[test]
fn blank_keepalive_lines_are_skipped() {
    let stream = "\n\n{\"type\":\"agent_start\"}\n\n";
    let mut reader = BufReader::new(stream.as_bytes());
    let record = read_record(&mut reader).unwrap().unwrap();
    assert_eq!(record, "{\"type\":\"agent_start\"}");
    assert!(read_record(&mut reader).unwrap().is_none());
}

// ---- recorded session ------------------------------------------------------

fn recorded() -> Vec<RpcRecord> {
    split_records(RECORDED)
        .into_iter()
        .map(|r| RpcRecord::parse(r).expect("every recorded record parses"))
        .collect()
}

#[test]
fn every_record_of_a_real_session_is_recognized() {
    let records = recorded();
    assert!(records.len() > 20, "fixture looks truncated");
    let unknown: Vec<_> = records
        .iter()
        .filter_map(|r| match r {
            RpcRecord::Unknown(v) => Some(v.get("type").cloned()),
            _ => None,
        })
        .collect();
    assert!(unknown.is_empty(), "unmodelled records: {unknown:?}");
}

#[test]
fn the_recorded_session_has_the_expected_event_shape() {
    let records = recorded();
    let events: Vec<&RpcEvent> = records
        .iter()
        .filter_map(|r| match r {
            RpcRecord::Event(e) => Some(e),
            _ => None,
        })
        .collect();

    assert!(events.iter().any(|e| matches!(e, RpcEvent::AgentStart)));
    assert!(events
        .iter()
        .any(|e| matches!(e, RpcEvent::AgentEnd { .. })));
    assert!(events.iter().any(|e| matches!(e, RpcEvent::TurnStart)));

    // The tool round-trip is the interesting part for the chat panel's tool
    // cards: start and end must correlate by toolCallId.
    let start = events
        .iter()
        .find_map(|e| match e {
            RpcEvent::ToolExecutionStart {
                tool_call_id,
                tool_name,
                ..
            } => Some((tool_call_id.clone(), tool_name.clone())),
            _ => None,
        })
        .expect("recorded session executes a tool");
    assert_eq!(start.1, "bash");
    let ended = events.iter().any(|e| matches!(
        e,
        RpcEvent::ToolExecutionEnd { tool_call_id, is_error: false, .. } if *tool_call_id == start.0
    ));
    assert!(ended, "tool_execution_end must correlate with its start");
}

#[test]
fn message_update_carries_assistant_message_event_not_delta() {
    // §4.2 sketched a `delta` field with text_delta/toolcall_* types. The real
    // field is `assistantMessageEvent`; modelling the sketch would have made
    // every streaming update an Unknown.
    let update = recorded()
        .into_iter()
        .find_map(|r| match r {
            RpcRecord::Event(RpcEvent::MessageUpdate {
                assistant_message_event,
                ..
            }) => Some(assistant_message_event),
            _ => None,
        })
        .expect("recorded session streams updates");
    assert!(
        update.get("type").and_then(|t| t.as_str()).is_some(),
        "the inner event carries its own type: {update}"
    );
}

#[test]
fn responses_correlate_by_id() {
    let responses: Vec<_> = recorded()
        .into_iter()
        .filter_map(|r| match r {
            RpcRecord::Response(resp) => Some(resp),
            _ => None,
        })
        .collect();
    assert_eq!(responses.len(), 3, "get_state, prompt, get_session_stats");
    assert!(responses.iter().all(|r| r.success));
    assert_eq!(responses[0].id.as_deref(), Some("c1"));
    assert_eq!(responses[0].command, "get_state");
    // get_state's payload is what the status bar renders.
    let state = responses[0].data.as_ref().unwrap();
    assert!(state.get("sessionId").is_some(), "{state}");
    assert!(state.get("isStreaming").is_some());
}

#[test]
fn an_unknown_record_type_degrades_instead_of_failing() {
    // A pi upgrade that adds an event must not kill a live session.
    let record = RpcRecord::parse(r#"{"type":"some_future_event","payload":1}"#).unwrap();
    assert!(matches!(record, RpcRecord::Unknown(_)));
}

// ---- commands --------------------------------------------------------------

#[test]
fn commands_serialize_to_the_documented_wire_shape() {
    let prompt = RpcCommand::Prompt {
        id: Some("c1".into()),
        message: "fix it".into(),
        streaming_behavior: Some(pi_client::StreamingBehavior::Steer),
    };
    let record = prompt.to_record().unwrap();
    assert!(record.ends_with('\n'), "commands must be framed records");
    let v: serde_json::Value = serde_json::from_str(record.trim_end()).unwrap();
    assert_eq!(v["type"], "prompt");
    assert_eq!(v["id"], "c1");
    assert_eq!(v["message"], "fix it");
    assert_eq!(v["streamingBehavior"], "steer");

    // Optional fields are omitted, not sent as null — pi validates shapes.
    let abort = RpcCommand::Abort { id: None }.to_record().unwrap();
    let v: serde_json::Value = serde_json::from_str(abort.trim_end()).unwrap();
    assert_eq!(v["type"], "abort");
    assert!(v.get("id").is_none(), "None must be omitted: {v}");

    let switch = RpcCommand::SwitchSession {
        id: None,
        session_path: "/s.jsonl".into(),
    };
    let v: serde_json::Value =
        serde_json::from_str(switch.to_record().unwrap().trim_end()).unwrap();
    assert_eq!(v["sessionPath"], "/s.jsonl", "camelCase on the wire");
}

// ---- extension UI (§4.3) ---------------------------------------------------

fn ui_request(method: &str) -> ExtensionUiRequest {
    serde_json::from_value(serde_json::json!({
        "type": "extension_ui_request", "id": "u1", "method": method,
        "title": "t", "message": "m"
    }))
    .unwrap()
}

#[test]
fn only_dialog_methods_block_the_agent() {
    // Fire-and-forget methods map to toasts. Treating them as blocking would
    // raise modals the agent never waited on.
    for method in ["select", "confirm", "input", "editor"] {
        assert!(ui_request(method).is_blocking(), "{method} blocks");
    }
    for method in [
        "notify",
        "setStatus",
        "setWidget",
        "setTitle",
        "set_editor_text",
    ] {
        assert!(!ui_request(method).is_blocking(), "{method} does not block");
    }
}

#[test]
fn ui_responses_serialize_per_method() {
    let v: serde_json::Value = serde_json::from_str(
        ExtensionUiResponse::value("u1", "b")
            .to_record()
            .unwrap()
            .trim_end(),
    )
    .unwrap();
    assert_eq!(v["type"], "extension_ui_response");
    assert_eq!(v["value"], "b");
    assert!(v.get("confirmed").is_none());

    let v: serde_json::Value = serde_json::from_str(
        ExtensionUiResponse::confirmed("u1", true)
            .to_record()
            .unwrap()
            .trim_end(),
    )
    .unwrap();
    assert_eq!(v["confirmed"], true);
    assert!(v.get("value").is_none());

    let v: serde_json::Value = serde_json::from_str(
        ExtensionUiResponse::cancelled("u1")
            .to_record()
            .unwrap()
            .trim_end(),
    )
    .unwrap();
    assert_eq!(v["cancelled"], true);
}

// ---- state derivation: what actually retires R8 ----------------------------

#[test]
fn the_recorded_session_settles_from_running_to_idle() {
    let mut tracker = SettleTracker::new();
    let mut transitions = Vec::new();
    for record in recorded() {
        if let Some(state) = tracker.observe(&record) {
            transitions.push(state);
        }
    }
    assert_eq!(transitions, vec![AgentState::Running, AgentState::Idle]);
}

#[test]
fn auto_retry_never_blips_idle_however_long_the_backoff() {
    // R8's first gap. The Phase 1 extension had to guess with a 750 ms
    // debounce, so a long provider backoff still showed idle mid-retry. RPC
    // states it outright, so no amount of elapsed time can produce a blip.
    let mut tracker = SettleTracker::new();
    let feed = |t: &mut SettleTracker, json: &str| t.observe(&RpcRecord::parse(json).unwrap());

    assert_eq!(
        feed(&mut tracker, r#"{"type":"agent_start"}"#),
        Some(AgentState::Running)
    );
    // Provider error: agent_end fires, but a retry is coming.
    feed(&mut tracker, r#"{"type":"agent_end","messages":[]}"#);
    let blip = feed(&mut tracker, r#"{"type":"auto_retry_start","attempt":1}"#);
    assert_eq!(
        tracker.state(),
        AgentState::Running,
        "must stay Running across the retry gap; transition was {blip:?}"
    );
    feed(
        &mut tracker,
        r#"{"type":"auto_retry_end","succeeded":true}"#,
    );
    feed(&mut tracker, r#"{"type":"agent_start"}"#);
    assert_eq!(tracker.state(), AgentState::Running);
    assert_eq!(
        feed(&mut tracker, r#"{"type":"agent_end","messages":[]}"#),
        Some(AgentState::Idle),
        "only a real end settles"
    );
}

#[test]
fn a_blocking_dialog_marks_the_agent_waiting_until_answered() {
    // R8's second gap: Phase 1 could only see dialogs opened through the
    // shared uiContext. Here pi reports every one, with an id to answer on.
    let mut tracker = SettleTracker::new();
    tracker.observe(&RpcRecord::parse(r#"{"type":"agent_start"}"#).unwrap());

    let confirm =
        r#"{"type":"extension_ui_request","id":"u1","method":"confirm","title":"t","message":"m"}"#;
    assert_eq!(
        tracker.observe(&RpcRecord::parse(confirm).unwrap()),
        Some(AgentState::Waiting)
    );
    assert!(tracker.is_blocked());
    assert_eq!(tracker.dialog_answered(), Some(AgentState::Running));
    assert!(!tracker.is_blocked());
}

#[test]
fn nested_dialogs_stay_waiting_until_the_last_one_closes() {
    let mut tracker = SettleTracker::new();
    tracker.observe(&RpcRecord::parse(r#"{"type":"agent_start"}"#).unwrap());
    for id in ["u1", "u2"] {
        let req = format!(
            r#"{{"type":"extension_ui_request","id":"{id}","method":"input","title":"t"}}"#
        );
        tracker.observe(&RpcRecord::parse(&req).unwrap());
    }
    assert_eq!(tracker.dialog_answered(), None, "still one dialog open");
    assert_eq!(tracker.state(), AgentState::Waiting);
    assert_eq!(tracker.dialog_answered(), Some(AgentState::Running));
}

#[test]
fn non_blocking_ui_requests_do_not_change_state() {
    let mut tracker = SettleTracker::new();
    tracker.observe(&RpcRecord::parse(r#"{"type":"agent_start"}"#).unwrap());
    let notify = r#"{"type":"extension_ui_request","id":"u9","method":"notify","message":"hi"}"#;
    assert_eq!(tracker.observe(&RpcRecord::parse(notify).unwrap()), None);
    assert_eq!(tracker.state(), AgentState::Running);
}
