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
        AgentKind::Grok => "Grok".to_string(),
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
        AgentKind::Grok => ("G", rgb(0xe6e6e6)),
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
        AgentKind::Grok => Some("grok"),
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

/// Every agent to offer, in the user's order (Settings › Agents, drag to
/// reorder). The single source the settings list, the ＋ menu and the
/// new-task default all read, so they can never drift.
///
/// With no saved order: every agent, built-in and custom together, sorted
/// alphabetically by display name (case-insensitive, ties by key). Agents the
/// saved order does not name — a built-in added in a later release, a newly
/// added custom agent — follow the ordered ones in that same base order.
pub(crate) fn agent_catalog(prefs: &Prefs) -> Vec<AgentKind> {
    let mut base: Vec<AgentKind> = AgentKind::known().to_vec();
    for (key, pref) in &prefs.agents {
        if pref.custom {
            base.push(AgentKind::Other(key.clone()));
        }
    }
    base.sort_by_cached_key(|k| (agent_display(k).to_lowercase(), k.as_str().to_string()));
    let mut out: Vec<AgentKind> = Vec::with_capacity(base.len());
    for key in &prefs.agent_order {
        if let Some(kind) = base.iter().find(|k| k.as_str() == key.as_str())
            && !out.contains(kind)
        {
            out.push(kind.clone());
        }
    }
    for kind in base {
        if !out.contains(&kind) {
            out.push(kind);
        }
    }
    out
}

/// The full agent order after dropping `dragged` onto `onto`'s row: every
/// catalog key, so the saved order is complete from the first drag on.
pub(crate) fn agent_order_after_move(prefs: &Prefs, dragged: &str, onto: &str) -> Vec<String> {
    let keys: Vec<String> = agent_catalog(prefs)
        .iter()
        .map(|k| k.as_str().to_string())
        .collect();
    reordered(&keys, dragged.to_string(), onto.to_string())
}

/// Agents the user has not turned off in Settings.
pub(crate) fn enabled_agents(prefs: &Prefs) -> Vec<AgentKind> {
    agent_catalog(prefs)
        .into_iter()
        .filter(|k| !prefs.agents.get(k.as_str()).is_some_and(|p| p.disabled))
        .collect()
}

/// Default agent for a new task, first match wins: the agent starred in
/// Settings (`prefs.default_agent`) if it exists and is enabled; else the last
/// used agent if still enabled; else the first enabled agent in the user's
/// order (Claude Code out of the box, the top of the alphabetical catalog).
pub(crate) fn default_enabled_agent(prefs: &Prefs) -> Option<AgentKind> {
    let enabled = enabled_agents(prefs);
    for key in [
        prefs.default_agent.as_deref(),
        prefs.last_task_agent.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        let kind = AgentKind::from(key);
        if enabled.contains(&kind) {
            return Some(kind);
        }
    }
    enabled.into_iter().next()
}

/// How an agent's star in Settings › Agents looks and behaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentStar {
    /// The effective default: a filled star, nothing to click.
    Filled,
    /// An enabled agent that is not the default: click to make it so.
    Outlined,
    /// A disabled agent can't be the default, so it has no star.
    Hidden,
}

/// The star state for `kind`'s row. Exactly one enabled agent is `Filled`:
/// the effective default, which may be the last-used or first agent rather
/// than an explicit star.
pub(crate) fn agent_star(prefs: &Prefs, kind: &AgentKind) -> AgentStar {
    if !enabled_agents(prefs).contains(kind) {
        AgentStar::Hidden
    } else if default_enabled_agent(prefs).as_ref() == Some(kind) {
        AgentStar::Filled
    } else {
        AgentStar::Outlined
    }
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
        AgentKind, AgentPref, Prefs, agent_catalog, agent_display, agent_order_after_move, default_enabled_agent,
        enabled_agents, valid_agent_slug, AgentStar, agent_star,
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
    fn the_base_order_is_alphabetical_by_display_name() {
        let mut prefs = with_custom("cursor-agent", "cursor-agent");
        prefs.agents.insert(
            "Zed".into(),
            AgentPref {
                custom: true,
                ..Default::default()
            },
        );
        let names: Vec<String> = agent_catalog(&prefs).iter().map(agent_display).collect();
        assert_eq!(
            names,
            [
                "Claude Code",
                "Codex",
                "cursor-agent",
                "Grok",
                "Kimi",
                "OpenCode",
                "pi",
                "Zed"
            ],
            "built-ins and custom agents together, case-insensitive"
        );
    }

    #[test]
    fn built_ins_alone_sort_alphabetically() {
        let names: Vec<String> = agent_catalog(&Prefs::default())
            .iter()
            .map(agent_display)
            .collect();
        assert_eq!(
            names,
            ["Claude Code", "Codex", "Grok", "Kimi", "OpenCode", "pi"]
        );
    }

    #[test]
    fn a_saved_partial_order_leads_then_the_alphabetical_remainder() {
        let mut prefs = with_custom("cursor-agent", "cursor-agent");
        prefs.agent_order = vec!["pi".into(), "kimi".into()];
        let keys: Vec<String> = agent_catalog(&prefs)
            .iter()
            .map(|k| k.as_str().to_string())
            .collect();
        assert_eq!(
            keys,
            [
                "pi",
                "kimi",
                "claude-code",
                "codex",
                "cursor-agent",
                "grok",
                "opencode"
            ]
        );
    }

    #[test]
    fn moving_an_agent_saves_the_full_order() {
        let prefs = Prefs::default();
        let order = agent_order_after_move(&prefs, "pi", "claude-code");
        assert_eq!(order[0], "pi", "dropped onto the top row");
        assert_eq!(order[1], "claude-code");
        assert_eq!(order.len(), AgentKind::known().len());
        let mut prefs = prefs;
        prefs.agent_order = order;
        assert_eq!(
            default_enabled_agent(&prefs),
            Some(AgentKind::Pi),
            "the top of the list is the default"
        );
    }

    #[test]
    fn the_default_is_the_first_enabled_agent_in_order() {
        let mut prefs = Prefs {
            agent_order: vec!["grok".into(), "pi".into()],
            ..Default::default()
        };
        prefs.agents.insert(
            "grok".into(),
            AgentPref {
                disabled: true,
                command: None,
                custom: false,
                signal: None,
            },
        );
        assert_eq!(
            default_enabled_agent(&prefs),
            Some(AgentKind::Pi),
            "a disabled agent at the top is skipped"
        );
        prefs.last_task_agent = Some("codex".into());
        assert_eq!(
            default_enabled_agent(&prefs),
            Some(AgentKind::Codex),
            "the last-used agent still wins"
        );
    }

    #[test]
    fn out_of_the_box_the_default_is_claude_code() {
        assert_eq!(
            default_enabled_agent(&Prefs::default()),
            Some(AgentKind::ClaudeCode)
        );
    }

    #[test]
    fn the_default_is_the_first_enabled_in_the_alphabetical_order() {
        let mut prefs = Prefs::default();
        prefs.agents.insert(
            "claude-code".into(),
            AgentPref {
                disabled: true,
                ..Default::default()
            },
        );
        assert_eq!(default_enabled_agent(&prefs), Some(AgentKind::Codex));
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
    fn default_task_agent_skips_a_disabled_top_agent() {
        let mut prefs = Prefs::default();
        prefs.agents.insert(
            "claude-code".to_string(),
            AgentPref {
                disabled: true,
                command: None,
                custom: false,
                signal: None,
            },
        );
        let kind = default_enabled_agent(&prefs).expect("other built-ins remain");
        assert_ne!(kind, AgentKind::ClaudeCode);
        assert!(!enabled_agents(&prefs).contains(&AgentKind::ClaudeCode));
    }

    #[test]
    fn default_task_agent_prefers_last_used_when_enabled() {
        let mut prefs = Prefs {
            last_task_agent: Some("claude-code".into()),
            ..Default::default()
        };
        assert_eq!(default_enabled_agent(&prefs), Some(AgentKind::ClaudeCode));
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

    #[test]
    fn a_starred_agent_beats_the_last_used_one() {
        let mut prefs = Prefs {
            default_agent: Some("kimi".into()),
            last_task_agent: Some("codex".into()),
            ..Default::default()
        };
        assert_eq!(default_enabled_agent(&prefs), Some(AgentKind::Kimi));
        prefs.default_agent = None;
        assert_eq!(
            default_enabled_agent(&prefs),
            Some(AgentKind::Codex),
            "no star: last used"
        );
        prefs.last_task_agent = None;
        assert_eq!(
            default_enabled_agent(&prefs),
            Some(AgentKind::ClaudeCode),
            "neither: first in catalog order"
        );
    }

    #[test]
    fn a_star_on_a_disabled_or_missing_agent_falls_back() {
        let mut prefs = Prefs {
            default_agent: Some("kimi".into()),
            last_task_agent: Some("codex".into()),
            ..Default::default()
        };
        prefs.agents.insert(
            "kimi".into(),
            AgentPref {
                disabled: true,
                ..Default::default()
            },
        );
        assert_eq!(
            default_enabled_agent(&prefs),
            Some(AgentKind::Codex),
            "disabled star falls to last used"
        );
        prefs.default_agent = Some("removed-custom".into());
        assert_eq!(
            default_enabled_agent(&prefs),
            Some(AgentKind::Codex),
            "a star on an agent that no longer exists falls back"
        );
        prefs.last_task_agent = None;
        assert_eq!(default_enabled_agent(&prefs), Some(AgentKind::ClaudeCode));
    }

    #[test]
    fn the_star_state_is_filled_outlined_or_hidden() {
        let mut prefs = Prefs {
            default_agent: Some("kimi".into()),
            ..Default::default()
        };
        prefs.agents.insert(
            "pi".into(),
            AgentPref {
                disabled: true,
                ..Default::default()
            },
        );
        assert_eq!(agent_star(&prefs, &AgentKind::Kimi), AgentStar::Filled);
        assert_eq!(agent_star(&prefs, &AgentKind::Codex), AgentStar::Outlined);
        assert_eq!(agent_star(&prefs, &AgentKind::Pi), AgentStar::Hidden);
        // Only one agent is filled, even with no explicit star.
        let plain = Prefs::default();
        assert_eq!(agent_star(&plain, &AgentKind::ClaudeCode), AgentStar::Filled);
        assert_eq!(agent_star(&plain, &AgentKind::Kimi), AgentStar::Outlined);
    }
}
