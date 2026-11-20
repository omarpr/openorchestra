//! How a tab looks: one icon and colour per kind, and the title-case name
//! tabs and the phone UI call each kind. Colours are fixed brand-style
//! values, the same in light and dark themes, so a tab is recognisable by
//! colour alone.

use super::*;

/// The tab kinds that have a look of their own. Agent tabs wear their own
/// brand mark (`agent_icon`); Review Overview keeps its list icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LookKind {
    Shell,
    Editor,
    Diff,
    // Constructed once the Browser tab lands.
    #[allow(dead_code)]
    Browser,
}

/// What a kind draws: a text glyph or a bundled svg icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KindMark {
    Glyph(&'static str),
    Icon(::ui::IconName),
}

pub(crate) struct KindLook {
    pub(crate) mark: KindMark,
    pub(crate) color: Rgba,
}

/// The icon and colour of each kind. Shell's `❯_` is cyan, clear of Codex
/// and OpenCode's greens and the diff's added-line green. The Editor (and
/// the file tabs it hosts) wear Zed's own mark in Zed blue. Diff's `⑂` is
/// pink. Browser is a globe; zed has no `Globe` icon at this rev, so
/// `ToolWeb` (a globe outline) stands in.
pub(crate) fn kind_look(kind: LookKind) -> KindLook {
    match kind {
        LookKind::Shell => KindLook {
            mark: KindMark::Glyph("❯_"),
            color: rgb(0x3ec7d6),
        },
        LookKind::Editor => KindLook {
            mark: KindMark::Icon(::ui::IconName::AiZed),
            color: rgb(0x3d8bfd),
        },
        LookKind::Diff => KindLook {
            mark: KindMark::Glyph("⑂"),
            color: rgb(0xe06c9f),
        },
        LookKind::Browser => KindLook {
            mark: KindMark::Icon(::ui::IconName::ToolWeb),
            color: rgb(0x61afef),
        },
    }
}

/// A kind's coloured mark, for a tab or a menu row. `size` is the svg's
/// edge; glyphs take their size from the text style.
pub(crate) fn kind_icon(hx: &HarnessTheme, kind: LookKind, size: f32) -> AnyElement {
    let look = kind_look(kind);
    match look.mark {
        KindMark::Icon(icon) => svg()
            .path(icon.path())
            .size(px(size))
            .text_color(look.color)
            .into_any_element(),
        KindMark::Glyph(glyph) => div()
            .text_xs()
            .not_italic()
            .font_family(hx.font_mono)
            .font_weight(gpui::FontWeight::BOLD)
            .text_color(look.color)
            .child(glyph)
            .into_any_element(),
    }
}

/// An agent's short tab name: "Claude", not "Claude Code". Custom agents go
/// by their slug.
pub(crate) fn agent_short_name(kind: &AgentKind) -> String {
    match kind {
        AgentKind::Pi => "Pi".to_string(),
        AgentKind::ClaudeCode => "Claude".to_string(),
        AgentKind::Codex => "Codex".to_string(),
        AgentKind::Kimi => "Kimi".to_string(),
        AgentKind::OpenCode => "OpenCode".to_string(),
        AgentKind::Grok => "Grok".to_string(),
        AgentKind::Other(s) => s.clone(),
    }
}

/// The name of a terminal tab: its agent's short name, or "Shell" for a
/// plain shell (including an agent tab restored after its session died).
pub(crate) fn terminal_label(agent: Option<&AgentKind>) -> String {
    match agent {
        Some(kind) => agent_short_name(kind),
        None => "Shell".to_string(),
    }
}

/// A browser tab's name: "Browser · <page title>", else the host, else
/// plain "Browser".
#[allow(dead_code)] // used by the Browser tab once it lands
pub(crate) fn browser_label(page_title: Option<&str>, host: Option<&str>) -> String {
    fn nonempty(s: Option<&str>) -> Option<&str> {
        s.map(str::trim).filter(|s| !s.is_empty())
    }
    match nonempty(page_title).or_else(|| nonempty(host)) {
        Some(detail) => format!("Browser · {detail}"),
        None => "Browser".to_string(),
    }
}

/// The name a tab shows. Stored titles ("claude", "❯_ shell") are kept as
/// is for saved panes and paired phones, but never shown for kinds that
/// have a name of their own; File and Review Overview show their stored
/// title (the file's name, the review's heading).
pub(crate) fn tab_label(tab: &Tab) -> String {
    match (&tab.content, &tab.agent) {
        (TabContent::Terminal(_), agent) => terminal_label(agent.as_ref()),
        (TabContent::Editor(_), _) => "Editor".to_string(),
        (TabContent::Diff, _) => "Diff".to_string(),
        (TabContent::Chat(_), _) => "pi chat".to_string(),
        (TabContent::File { .. }, _) | (TabContent::ReviewOverview(_), _) => tab.title.clone(),
    }
}

/// Whether a tab wears the running spinner: its agent is working, or it is a
/// Review Overview whose review is generating.
pub(crate) fn tab_running(agent_running: bool, overview_running: bool) -> bool {
    agent_running || overview_running
}

/// A Review Overview tab whose review is generating.
pub(crate) fn overview_running(tab: &Tab) -> bool {
    matches!(&tab.content, TabContent::ReviewOverview(o) if matches!(o.status, OverviewStatus::Running(_)))
}

/// The running spinner a tab shows after its icon, shared by agent tabs and
/// the Review Overview so the two cannot drift.
pub(crate) fn tab_running_spinner(hx: &HarnessTheme, tab_id: u64, t: f32) -> impl IntoElement {
    div()
        .id(("tab-running", tab_id as usize))
        .flex_none()
        .tooltip(tip("Running"))
        .child(spin_arc(hx.accent, tint(hx.accent, 0.3), 10.0, t))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_spinner_shows_for_agent_or_overview() {
        assert!(!tab_running(false, false));
        assert!(tab_running(true, false));
        assert!(tab_running(false, true));
        assert!(tab_running(true, true));
    }

    #[test]
    fn agent_tabs_use_short_title_case_names() {
        assert_eq!(agent_short_name(&AgentKind::ClaudeCode), "Claude");
        assert_eq!(agent_short_name(&AgentKind::Pi), "Pi");
        assert_eq!(agent_short_name(&AgentKind::OpenCode), "OpenCode");
        assert_eq!(
            agent_short_name(&AgentKind::Other("cursor-agent".into())),
            "cursor-agent"
        );
    }

    #[test]
    fn plain_terminals_are_shells() {
        assert_eq!(terminal_label(None), "Shell");
        assert_eq!(terminal_label(Some(&AgentKind::Codex)), "Codex");
    }

    #[test]
    fn browser_label_prefers_title_then_host() {
        assert_eq!(
            browser_label(Some("Vite App"), Some("localhost")),
            "Browser · Vite App"
        );
        assert_eq!(
            browser_label(Some("  "), Some("localhost")),
            "Browser · localhost"
        );
        assert_eq!(browser_label(None, None), "Browser");
    }

    #[test]
    fn kind_colors_match_spec() {
        assert_eq!(kind_look(LookKind::Shell).color, rgb(0x3ec7d6));
        assert_eq!(kind_look(LookKind::Editor).color, rgb(0x3d8bfd));
        assert_eq!(kind_look(LookKind::Diff).color, rgb(0xe06c9f));
        assert_eq!(kind_look(LookKind::Browser).color, rgb(0x61afef));
    }
}
