//! The model menus use each CLI's own catalog so model IDs and aliases stay current.

use super::{CLAUDE_REVIEW_ARGS, PI_REVIEW_ARGS, ReviewCli, RunControl};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::AtomicBool};
use std::time::{Duration, Instant};

const CLAUDE_INITIALIZE: &[u8] =
    b"{\"type\":\"control_request\",\"request_id\":\"models\",\"request\":{\"subtype\":\"initialize\"}}\n";
const PI_GET_MODELS: &[u8] = b"{\"id\":\"models\",\"type\":\"get_available_models\"}\n";
const CATALOG_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ReviewModel {
    pub(crate) id: String,
    pub(crate) label: String,
}

pub(crate) fn cache_path(state_dir: &Path, cli: ReviewCli) -> PathBuf {
    let name = match cli {
        ReviewCli::Codex => "review-models-codex.json",
        ReviewCli::ClaudeCode => "review-models-claude-code.json",
        ReviewCli::Pi => "review-models-pi.json",
        ReviewCli::Grok => "review-models-grok.json",
    };
    state_dir.join(name)
}

/// The last catalog a Reload models action stored. Missing or unreadable
/// means the menu stays empty until the user asks again.
pub(crate) fn read_cache(state_dir: &Path, cli: ReviewCli) -> Option<Vec<ReviewModel>> {
    let bytes = std::fs::read(cache_path(state_dir, cli)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub(crate) fn write_cache(
    state_dir: &Path,
    cli: ReviewCli,
    models: &[ReviewModel],
) -> std::io::Result<()> {
    let path = cache_path(state_dir, cli);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec(models).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
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
    // Spawned directly: npm's extensionless sh wrapper won't run on Windows.
    let executable = os_compat::Os::CURRENT.direct_launcher(&executable);
    let mut command = os_compat::process::command(executable);
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
        ReviewCli::Grok => {
            command.arg("models");
            None
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
        ReviewCli::Grok => return decode_grok_models(output),
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

fn decode_grok_models(output: &[u8]) -> Result<Vec<ReviewModel>, String> {
    let text = String::from_utf8_lossy(output);
    let mut seen = HashSet::new();
    let options: Vec<_> = text
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let rest = line
                .strip_prefix('*')
                .or_else(|| line.strip_prefix('-'))?
                .trim();
            let id = rest.split_whitespace().next()?.trim();
            if id.is_empty() || !seen.insert(id.to_string()) {
                return None;
            }
            let label = if rest.contains("(default)") {
                format!("{id} (default)")
            } else {
                id.to_string()
            };
            Some(ReviewModel {
                id: id.to_string(),
                label,
            })
        })
        .collect();
    if options.is_empty() {
        return Err("Grok returned an unreadable model catalog.".to_string());
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
    fn a_saved_catalog_round_trips_and_a_missing_file_stays_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_cache(dir.path(), ReviewCli::Codex).is_none());
        let models = vec![ReviewModel {
            id: "visible-model".into(),
            label: "Visible Model".into(),
        }];
        write_cache(dir.path(), ReviewCli::Codex, &models).unwrap();
        assert_eq!(
            read_cache(dir.path(), ReviewCli::Codex).as_deref(),
            Some(models.as_slice())
        );
        assert!(read_cache(dir.path(), ReviewCli::Pi).is_none());
        std::fs::write(cache_path(dir.path(), ReviewCli::ClaudeCode), b"not-json").unwrap();
        assert!(read_cache(dir.path(), ReviewCli::ClaudeCode).is_none());
    }

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

    #[test]
    fn grok_catalog_reads_the_text_list_and_marks_the_default() {
        let models = decode(
            ReviewCli::Grok,
            b"You are logged in with grok.com.\n\nDefault model: grok-4.7\n\nAvailable models:\n  * grok-4.7 (default)\n  - grok-4.7-build-fast\n  - grok-4.6\n  * grok-4.7 (default)\n",
        )
        .unwrap();
        assert_eq!(
            models
                .iter()
                .map(|model| (model.id.as_str(), model.label.as_str()))
                .collect::<Vec<_>>(),
            [
                ("grok-4.7", "grok-4.7 (default)"),
                ("grok-4.7-build-fast", "grok-4.7-build-fast"),
                ("grok-4.6", "grok-4.6"),
            ]
        );
        assert!(decode(ReviewCli::Grok, b"Default model: grok-4.7\n").is_err());
    }
}
