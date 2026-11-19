//! Agent Card builders for member specialists and the home orchestrator.

use serde::{Deserialize, Serialize};

use crate::{encode_tenant, MemberEndpoint};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentSkill {
    pub id: String,
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentCard {
    pub name: String,
    pub description: String,
    pub version: String,
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    pub url: String,
    #[serde(rename = "preferredTransport")]
    pub preferred_transport: String,
    pub skills: Vec<AgentSkill>,
    pub capabilities: Capabilities,
    #[serde(rename = "securitySchemes")]
    pub security_schemes: serde_json::Value,
    pub security: serde_json::Value,
    #[serde(rename = "defaultInputModes")]
    pub default_input_modes: Vec<String>,
    #[serde(rename = "defaultOutputModes")]
    pub default_output_modes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Capabilities {
    pub streaming: bool,
}

fn security_schemes() -> serde_json::Value {
    serde_json::json!({
        "apiKey": {
            "type": "apiKey",
            "in": "header",
            "name": "Authorization",
            "description": "Bearer token (Authorization: Bearer <key>)"
        }
    })
}

fn security() -> serde_json::Value {
    serde_json::json!([{ "apiKey": [] }])
}

pub fn agent_card_for_member(
    base_url: &str,
    task_name: &str,
    task_slug: &str,
    member: &MemberEndpoint,
) -> AgentCard {
    let role = if member.is_coordinator {
        "orchestrator"
    } else {
        "specialist"
    };
    let enc = encode_tenant(&member.worktree_id);
    let url = format!("{base_url}/t/{enc}/jsonrpc");
    AgentCard {
        name: format!("openOrchestra · {}", member.repo),
        description: format!(
            "{role} for `{}` worktree in task `{task_slug}` ({task_name})",
            member.repo
        ),
        version: "1.0".into(),
        protocol_version: "1.0".into(),
        url,
        preferred_transport: "JSONRPC".into(),
        skills: vec![AgentSkill {
            id: "repo-specialize".into(),
            name: format!("Edit & review {}", member.repo),
            description: format!(
                "Implement and review changes only in the `{}` worktree at `{}`.",
                member.repo,
                member.path.display()
            ),
            tags: vec![format!("repo:{}", member.repo), format!("role:{role}")],
        }],
        capabilities: Capabilities { streaming: true },
        security_schemes: security_schemes(),
        security: security(),
        default_input_modes: vec!["text/plain".into()],
        default_output_modes: vec!["text/plain".into()],
        metadata: Some(serde_json::json!({
            "openOrchestra": {
                "worktreeId": member.worktree_id,
                "repo": member.repo,
                "path": member.path,
                "isCoordinator": member.is_coordinator,
                "taskSlug": task_slug,
            }
        })),
    }
}

pub fn orchestrator_card(
    base_url: &str,
    task_name: &str,
    task_slug: &str,
    home: &MemberEndpoint,
    siblings: &[MemberEndpoint],
) -> AgentCard {
    let mut card = agent_card_for_member(base_url, task_name, task_slug, home);
    card.skills[0].id = "orchestrate".into();
    card.skills[0].name = format!("Orchestrate task {task_slug}");
    card.skills[0].description =
        "Delegate work to sibling specialist agents via A2A; do not edit sibling trees directly."
            .into();
    card.skills[0].tags = vec![format!("repo:{}", home.repo), "role:orchestrator".into()];
    let sibling_urls: Vec<String> = siblings
        .iter()
        .filter(|m| !m.is_coordinator)
        .map(|m| {
            format!(
                "{base_url}/t/{}/.well-known/agent-card.json",
                encode_tenant(&m.worktree_id)
            )
        })
        .collect();
    if let Some(meta) = card.metadata.as_mut() {
        if let Some(obj) = meta.as_object_mut() {
            if let Some(oo) = obj.get_mut("openOrchestra").and_then(|v| v.as_object_mut()) {
                oo.insert(
                    "siblingCards".into(),
                    serde_json::Value::Array(
                        sibling_urls
                            .into_iter()
                            .map(serde_json::Value::String)
                            .collect(),
                    ),
                );
            }
        }
    }
    card
}
