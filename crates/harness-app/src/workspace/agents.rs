//! The agent catalog: built-ins plus user-defined agents from prefs, and
//! which of them are enabled.

use super::*;

/// The name an agent goes by in front of users — notification titles, rows.
///
/// Custom agents (`Other`) go by their own slug so a notification names the
/// actual CLI the user added rather than a generic "Agent".
pub(crate) fn agent_display(kind: &AgentKind) -> String {
    match kind {
        AgentKind::Pi => "pi".to_string(),
        AgentKind::ClaudeCode => "Claude Code".to_string(),
        AgentKind::Codex => "Codex".to_string(),
        AgentKind::Kimi => "Kimi".to_string(),
        AgentKind::OpenCode => "OpenCode".to_string(),
        AgentKind::Other(s) => s.clone(),
    }
}

/// An agent's fallback monogram and color, used for custom agents without a
/// bundled brand logo.
pub(crate) fn agent_mark(kind: &AgentKind) -> (&'static str, Rgba) {
    match kind {
        AgentKind::Pi => ("π", rgb(0x8b7cf6)),
        AgentKind::ClaudeCode => ("✱", rgb(0xd97757)),
        AgentKind::Codex => ("⬡", rgb(0x10a37f)),
        AgentKind::Kimi => ("K", rgb(0x4f9cf9)),
        AgentKind::OpenCode => ("⟡", rgb(0x3fb950)),
        AgentKind::Other(_) => ("◇", rgb(0x9aa3af)),
    }
}

/// Render a bundled agent brand mark, falling back to the custom-agent
/// monogram where the catalog has no logo.
pub(crate) fn agent_icon(kind: &AgentKind, size: f32) -> AnyElement {
    let (glyph, color) = agent_mark(kind);
    let logo = match kind {
        AgentKind::Pi => Some("pi"),
        AgentKind::ClaudeCode => Some("claudecode"),
        AgentKind::Codex => Some("codex"),
        AgentKind::Kimi => Some("kimi"),
        AgentKind::OpenCode => Some("opencode"),
        AgentKind::Other(slug) if matches!(slug.as_str(), "cursor" | "cursor-agent") => {
            Some("cursor")
        }
        AgentKind::Other(_) => None,
    };

    if let Some(logo) = logo {
        svg()
            .path(format!("icons/oo/agents/{logo}.svg"))
            .size(px(size))
            .text_color(color)
            .into_any_element()
    } else {
        div()
            .size(px(size))
            .flex()
            .items_center()
            .justify_center()
            .text_xs()
            .font_weight(gpui::FontWeight::BOLD)
            .text_color(color)
            .child(glyph)
            .into_any_element()
    }
}

/// Every agent to offer, built-ins first (in sidebar order) then user-added
/// custom agents by key. The single source both the settings list and the ＋
/// menu iterate, so the two can never drift.
pub(crate) fn agent_catalog(prefs: &Prefs) -> Vec<AgentKind> {
    let mut out: Vec<AgentKind> = AgentKind::known().to_vec();
    for (key, pref) in &prefs.agents {
        if pref.custom {
            out.push(AgentKind::Other(key.clone()));
        }
    }
    out
}

/// Agents the user has not turned off in Settings.
pub(crate) fn enabled_agents(prefs: &Prefs) -> Vec<AgentKind> {
    agent_catalog(prefs)
        .into_iter()
        .filter(|k| {
            !prefs
                .agents
                .get(k.as_str())
                .is_some_and(|p| p.disabled)
        })
        .collect()
}

/// Default agent for a new task: last used (if still enabled), else OpenCode
/// when enabled (D12), else the first enabled catalog entry.
pub(crate) fn default_enabled_agent(prefs: &Prefs) -> Option<AgentKind> {
    let enabled = enabled_agents(prefs);
    if enabled.is_empty() {
        return None;
    }
    if let Some(key) = prefs.last_task_agent.as_deref() {
        let kind = AgentKind::from(key);
        if enabled.iter().any(|k| k == &kind) {
            return Some(kind);
        }
    }
    enabled
        .iter()
        .find(|k| matches!(k, AgentKind::OpenCode))
        .cloned()
        .or_else(|| enabled.into_iter().next())
}

/// Whether `slug` is a usable key for a new custom agent: non-empty, only
/// `[A-Za-z0-9_-]`, and not already taken by a built-in kind or an existing
/// entry (custom or a built-in override).
pub(crate) fn valid_agent_slug(slug: &str, prefs: &Prefs) -> bool {
    if slug.is_empty() {
        return false;
    }
    if !slug
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return false;
    }
    if AgentKind::known().iter().any(|k| k.as_str() == slug) {
        return false;
    }
    !prefs.agents.contains_key(slug)
}

/// Per-agent user preferences (settings screen). Every field defaults to the
/// permissive state so a missing entry behaves like a fresh install.
#[derive(Debug, serde::Serialize, serde::Deserialize, Clone, Default)]
pub(crate) struct AgentPref {
    #[serde(default)]
    pub(crate) disabled: bool,
    /// Launch-command override, run verbatim via `$SHELL -lc`. `None` = the
    /// adapter's own resolved command.
    #[serde(default)]
    pub(crate) command: Option<String>,
    /// `true` for user-added agents. Distinguishes a real custom entry (an
    /// `AgentKind::Other` the user created) from a sparse override entry for a
    /// built-in kind, so resetting built-ins never deletes custom agents.
    #[serde(default)]
    pub(crate) custom: bool,
    /// Optional signal profile for custom agents: fidelity + hook-install
    /// dialect. `None` = terminal-only (no pills/notifications from hooks).
    #[serde(default)]
    pub(crate) signal: Option<agent_runtime::SignalProfile>,
}

#[cfg(test)]
mod agent_prefs_tests {
    use super::{
        AgentKind, AgentPref, Prefs, agent_catalog, default_enabled_agent, enabled_agents,
        valid_agent_slug,
    };

    fn with_custom(slug: &str, command: &str) -> Prefs {
        let mut prefs = Prefs::default();
        prefs.agents.insert(
            slug.to_string(),
            AgentPref {
                disabled: false,
                command: Some(command.to_string()),
                custom: true,
                signal: None,
            },
        );
        prefs
    }

    #[test]
    fn a_custom_agent_survives_a_json_round_trip() {
        let prefs = with_custom("cursor-agent", "cursor-agent");
        let json = serde_json::to_string(&prefs).unwrap();
        let back: Prefs = serde_json::from_str(&json).unwrap();
        let entry = back
            .agents
            .get("cursor-agent")
            .expect("custom entry persists");
        assert!(entry.custom, "custom flag round-trips");
        assert_eq!(entry.command.as_deref(), Some("cursor-agent"));
        assert!(entry.signal.is_none(), "no signal by default");
    }

    #[test]
    fn a_signal_profile_round_trips_on_a_custom_agent() {
        use agent_runtime::{HookInstall, SignalProfile, WaitFidelity};
        let mut prefs = with_custom("cursor-agent", "cursor-agent");
        prefs.agents.get_mut("cursor-agent").unwrap().signal = Some(SignalProfile {
            fidelity: WaitFidelity::Exact,
            install: HookInstall::ClaudeLocalSettings,
        });
        let json = serde_json::to_string(&prefs).unwrap();
        assert!(
            json.contains("\"exact\""),
            "fidelity serializes as snake_case"
        );
        assert!(
            json.contains("\"claude_local_settings\""),
            "install dialect serializes as snake_case"
        );
        let back: Prefs = serde_json::from_str(&json).unwrap();
        let signal = back
            .agents
            .get("cursor-agent")
            .and_then(|a| a.signal.clone())
            .expect("signal persists");
        assert_eq!(signal.fidelity, WaitFidelity::Exact);
        assert_eq!(signal.install, HookInstall::ClaudeLocalSettings);
    }

    #[test]
    fn the_catalog_is_the_built_ins_then_the_custom_agents() {
        let prefs = with_custom("cursor-agent", "cursor-agent");
        let catalog = agent_catalog(&prefs);
        assert_eq!(
            &catalog[..5],
            &AgentKind::known()[..],
            "built-ins come first, in order"
        );
        assert_eq!(
            catalog.last(),
            Some(&AgentKind::Other("cursor-agent".to_string())),
            "the custom agent is appended",
        );
    }

    #[test]
    fn a_built_in_override_never_shows_up_as_a_catalog_agent() {
        // Disabling a built-in writes a sparse, non-custom entry — it must not
        // be mistaken for a user-added agent.
        let mut prefs = Prefs::default();
        prefs.agents.insert(
            "pi".to_string(),
            AgentPref {
                disabled: true,
                command: None,
                custom: false,
                signal: None,
            },
        );
        assert_eq!(agent_catalog(&prefs).len(), AgentKind::known().len());
    }

    #[test]
    fn slug_validation_rejects_bad_names_and_collisions() {
        let prefs = with_custom("cursor-agent", "cursor-agent");
        assert!(valid_agent_slug("aider", &prefs), "a fresh slug is fine");
        assert!(
            valid_agent_slug("my_agent-2", &prefs),
            "underscores and digits are fine"
        );
        assert!(!valid_agent_slug("", &prefs), "empty is rejected");
        assert!(
            !valid_agent_slug("has space", &prefs),
            "spaces are rejected"
        );
        assert!(
            !valid_agent_slug("bad/slash", &prefs),
            "slashes are rejected"
        );
        assert!(
            !valid_agent_slug("pi", &prefs),
            "a built-in kind cannot be re-added"
        );
        assert!(
            !valid_agent_slug("cursor-agent", &prefs),
            "an existing custom cannot be re-added"
        );
    }

    #[test]
    fn resetting_built_ins_keeps_custom_agents() {
        let mut prefs = with_custom("cursor-agent", "cursor-agent");
        prefs.agents.insert(
            "codex".to_string(),
            AgentPref {
                disabled: true,
                command: Some("codex --foo".into()),
                custom: false,
                signal: None,
            },
        );
        // Mirrors the settings "reset" action.
        prefs.agents.retain(|_, a| a.custom);
        assert!(
            prefs.agents.contains_key("cursor-agent"),
            "custom agent stays"
        );
        assert!(
            !prefs.agents.contains_key("codex"),
            "built-in override is dropped"
        );
    }

    #[test]
    fn default_task_agent_skips_disabled_opencode() {
        let mut prefs = Prefs::default();
        prefs.agents.insert(
            "opencode".to_string(),
            AgentPref {
                disabled: true,
                command: None,
                custom: false,
                signal: None,
            },
        );
        let kind = default_enabled_agent(&prefs).expect("other built-ins remain");
        assert_ne!(kind, AgentKind::OpenCode);
        assert!(!enabled_agents(&prefs).contains(&AgentKind::OpenCode));
    }

    #[test]
    fn default_task_agent_prefers_last_used_when_enabled() {
        let mut prefs = Prefs { last_task_agent: Some("claude-code".into()), ..Default::default() };
        assert_eq!(
            default_enabled_agent(&prefs),
            Some(AgentKind::ClaudeCode)
        );
        prefs.agents.insert(
            "claude-code".into(),
            AgentPref {
                disabled: true,
                command: None,
                custom: false,
                signal: None,
            },
        );
        assert_ne!(
            default_enabled_agent(&prefs),
            Some(AgentKind::ClaudeCode),
            "disabled last-used falls through"
        );
    }
}
