//! The app's asset source: zed's embedded assets plus the few icons this app
//! draws that zed's set doesn't have (`icons/oo/…`).
//!
//! Ours live under their own `icons/oo/` prefix so they can never shadow a
//! zed icon of the same name, which the hosted workspace would then draw too.

use std::borrow::Cow;

use gpui::{AssetSource, SharedString};

/// Icons bundled with the app itself, by asset path.
const OWN: &[(&str, &[u8])] = &[
    ("icons/oo/house.svg", include_bytes!("../icons/house.svg")),
    ("icons/oo/devices.svg", include_bytes!("../icons/devices.svg")),
    (
        "icons/oo/agents/claudecode.svg",
        include_bytes!("../icons/agents/claudecode.svg"),
    ),
    (
        "icons/oo/agents/codex.svg",
        include_bytes!("../icons/agents/codex.svg"),
    ),
    (
        "icons/oo/agents/cursor.svg",
        include_bytes!("../icons/agents/cursor.svg"),
    ),
    (
        "icons/oo/agents/kimi.svg",
        include_bytes!("../icons/agents/kimi.svg"),
    ),
    (
        "icons/oo/agents/opencode.svg",
        include_bytes!("../icons/agents/opencode.svg"),
    ),
    (
        "icons/oo/agents/pi.svg",
        include_bytes!("../icons/agents/pi.svg"),
    ),
];

pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, bytes)) = OWN.iter().find(|(p, _)| *p == path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        let mut out = assets::Assets.list(path)?;
        out.extend(OWN.iter().filter(|(p, _)| p.starts_with(path)).map(|(p, _)| SharedString::from(*p)));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serves_its_own_icons_and_zeds() {
        assert!(AppAssets.load("icons/oo/house.svg").unwrap().is_some());
        assert!(AppAssets.load("icons/git_branch.svg").unwrap().is_some());
        assert!(AppAssets.list("icons/oo").unwrap().iter().any(|p| p.as_ref() == "icons/oo/house.svg"));
    }
}
