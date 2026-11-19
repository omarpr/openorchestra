//! The model menus use each CLI's own catalog so model IDs and aliases stay current.

use super::{CLAUDE_REVIEW_ARGS, PI_REVIEW_ARGS, ReviewCli, RunControl};
use serde_json::Value;
use std::collections::HashSet;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, atomic::AtomicBool};
use std::time::{Duration, Instant};

const CLAUDE_INITIALIZE: &[u8] =
    b"{\"type\":\"control_request\",\"request_id\":\"models\",\"request\":{\"subtype\":\"initialize\"}}\n";
const PI_GET_MODELS: &[u8] = b"{\"id\":\"models\",\"type\":\"get_available_models\"}\n";
const CATALOG_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct ReviewModel {
    pub(crate) id: String,
    pub(crate) label: String,
}

pub(crate) enum ModelCatalog {
    Loading,
    Ready(Vec<ReviewModel>),
    Failed(String),
}

pub(crate) fn discover(cli: ReviewCli, cwd: &Path) -> Result<Vec<ReviewModel>, String> {
    let program = cli.program();
    let executable = agent_runtime::resolve_on_path(program)
        .ok_or_else(|| format!("Could not find `{program}` on PATH."))?;
    let mut command = Command::new(executable);
    command.current_dir(cwd);
    let input = match cli {
        ReviewCli::Codex => {
            command.args(["--disable", "plugins", "debug", "models"]);
            None
        }
        ReviewCli::ClaudeCode => {
            command.args(CLAUDE_REVIEW_ARGS).args([
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--verbose",
            ]);
            Some(CLAUDE_INITIALIZE)
        }
        ReviewCli::Pi => {
            command.args(PI_REVIEW_ARGS).args(["--mode", "rpc"]);
            Some(PI_GET_MODELS)
        }
    };

    let run = RunControl {
        cancelled: Arc::new(AtomicBool::new(false)),
        deadline: Instant::now() + Duration::from_secs(10),
    };
    let output = run
        .capture(&mut command, input, CATALOG_BYTES)?
        .checked(&format!("Read {} models", cli.label()), true)?;
    decode(cli, &output.bytes)
}

fn decode(cli: ReviewCli, output: &[u8]) -> Result<Vec<ReviewModel>, String> {
    let response;
    let (rows, id_field, label_field) = match cli {
        ReviewCli::Codex => {
            response = serde_json::from_slice::<Value>(output)
                .map_err(|error| format!("Could not read the Codex model catalog. {error}"))?;
            (response.get("models"), "slug", "display_name")
        }
        ReviewCli::ClaudeCode => {
            response = json_lines(output)
                .find(|reply| {
                    reply.get("type").and_then(Value::as_str) == Some("control_response")
                        && reply
                            .pointer("/response/request_id")
                            .and_then(Value::as_str)
                            == Some("models")
                })
                .ok_or("Claude Code did not return its model catalog.")?;
            (
                response.pointer("/response/response/models"),
                "value",
                "displayName",
            )
        }
        ReviewCli::Pi => {
            response = json_lines(output)
                .find(|reply| {
                    reply.get("type").and_then(Value::as_str) == Some("response")
                        && reply.get("id").and_then(Value::as_str) == Some("models")
                })
                .ok_or("pi did not return its model catalog.")?;
            (response.pointer("/data/models"), "id", "name")
        }
    };
    let rows = rows
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{} returned an unreadable model catalog.", cli.label()))?;
    let mut seen = HashSet::new();
    let mut options: Vec<_> = rows
        .iter()
        .filter_map(|row| {
            let id = row.get(id_field)?.as_str()?.trim();
            if id.is_empty() || (cli == ReviewCli::ClaudeCode && id == "default") {
                return None;
            }
            if cli == ReviewCli::Codex
                && row
                    .get("visibility")
                    .and_then(Value::as_str)
                    .is_some_and(|v| v != "list")
            {
                return None;
            }
            let label = row
                .get(label_field)
                .and_then(Value::as_str)
                .filter(|label| !label.trim().is_empty())
                .unwrap_or(id);
            let option = if cli == ReviewCli::Pi {
                let provider = row.get("provider")?.as_str()?;
                ReviewModel {
                    id: format!("{provider}/{id}"),
                    label: format!("{provider} · {label}"),
                }
            } else {
                ReviewModel {
                    id: id.to_string(),
                    label: label.to_string(),
                }
            };
            seen.insert(option.id.clone()).then_some(option)
        })
        .collect();
    if cli == ReviewCli::Pi {
        options.sort_by(|a, b| a.label.cmp(&b.label).then_with(|| a.id.cmp(&b.id)));
    }
    Ok(options)
}

fn json_lines(output: &[u8]) -> impl Iterator<Item = Value> + '_ {
    output
        .split(|&byte| byte == b'\n')
        .filter_map(|line| serde_json::from_slice(line).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_catalog_filters_hidden_models() {
        let models = decode(
            ReviewCli::Codex,
            br#"{"models":[
            {"slug":"visible-model","display_name":"Visible Model","visibility":"list"},
            {"slug":"hidden-model","display_name":"Hidden Model","visibility":"hide"}
        ]}"#,
        )
        .unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "visible-model");
        assert_eq!(models[0].label, "Visible Model");
    }

    #[test]
    fn claude_catalog_uses_the_selectable_value_and_keeps_one_default() {
        let models = decode(
            ReviewCli::ClaudeCode,
            serde_json::json!({"type":"control_response","response":{
            "request_id":"models","response":{"models":[
                {"value":"default","displayName":"Default"},
                {"value":"opus","displayName":"Opus","resolvedModel":"claude-opus"}
            ]}
            }})
            .to_string()
            .as_bytes(),
        )
        .unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "opus");
    }

    #[test]
    fn pi_preserves_equal_model_ids_from_different_providers() {
        let models = decode(
            ReviewCli::Pi,
            serde_json::json!({"type":"response","id":"models","data":{"models":[
                {"id":"shared","provider":"provider-b","name":"Shared"},
                {"id":"shared","provider":"provider-a","name":"Shared"}
            ]}})
            .to_string()
            .as_bytes(),
        )
        .unwrap();
        assert_eq!(
            models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["provider-a/shared", "provider-b/shared"]
        );
    }
}
