//! The languages the hosted editor highlights: zed's own configs and queries
//! (the `grammars` crate) paired with grammars we compile in.
//!
//! A curated set, not one, not every grammar zed ships (§3.2 revised). Each
//! tuple is (config name in the `grammars` crate, grammar), mapped exactly as
//! zed's `grammars::native_grammars` does: javascript uses the TSX grammar,
//! jsonc the JSON one, env the Bash one, and markdown is two grammars (block
//! and inline, the second injected into the first).
//!
//! Grammar versions are pinned in Cargo.toml to zed's own Cargo.lock, because
//! the queries compile against the node types a grammar produces; a mismatch
//! panics at startup. `every_curated_language_compiles` catches that in tests.

use tree_sitter_language::LanguageFn;

pub fn curated() -> Vec<(&'static str, LanguageFn)> {
    vec![
        ("rust", tree_sitter_rust::LANGUAGE),
        ("javascript", tree_sitter_typescript::LANGUAGE_TSX),
        ("typescript", tree_sitter_typescript::LANGUAGE_TYPESCRIPT),
        ("tsx", tree_sitter_typescript::LANGUAGE_TSX),
        ("go", tree_sitter_go::LANGUAGE),
        ("json", tree_sitter_json::LANGUAGE),
        ("python", tree_sitter_python::LANGUAGE),
        ("yaml", tree_sitter_yaml::LANGUAGE),
        ("css", tree_sitter_css::LANGUAGE),
        ("bash", tree_sitter_bash::LANGUAGE),
        ("env", tree_sitter_bash::LANGUAGE),
        ("jsonc", tree_sitter_json::LANGUAGE),
        ("markdown", tree_sitter_md::LANGUAGE),
        ("markdown-inline", tree_sitter_md::INLINE_LANGUAGE),
        ("c", tree_sitter_c::LANGUAGE),
        ("cpp", tree_sitter_cpp::LANGUAGE),
        ("diff", tree_sitter_diff::LANGUAGE),
        ("gitcommit", tree_sitter_gitcommit::LANGUAGE),
        ("gomod", tree_sitter_go_mod::LANGUAGE),
        ("gowork", tree_sitter_gowork::LANGUAGE),
        ("jsdoc", tree_sitter_jsdoc::LANGUAGE),
        ("regex", tree_sitter_regex::LANGUAGE),
    ]
}

/// One curated language, ready to add to a `LanguageRegistry`.
pub fn build(name: &str, grammar: LanguageFn) -> anyhow::Result<language::Language> {
    language::Language::new(grammars::load_config(name), Some(grammar.into()))
        .with_queries(grammars::load_queries(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_curated_language_compiles() {
        for (name, grammar) in curated() {
            if let Err(e) = build(name, grammar) {
                panic!("{name}: queries don't compile against the pinned grammar: {e}");
            }
        }
    }

    #[test]
    fn curated_set_matches_the_pinned_zed_snapshot() {
        // A snapshot of every config in zed's `grammars` crate at the pinned
        // rev, except its internal `zed-keybind-context`. Exact, so dropping
        // one fails. It can't see a language a newer zed adds: when the zed
        // pin moves, compare `want` with the upstream directories
        // (crates/grammars/src/*/config.toml) and update both lists.
        let mut want = vec![
            "bash", "c", "cpp", "css", "diff", "env", "gitcommit", "go", "gomod", "gowork",
            "javascript", "jsdoc", "json", "jsonc", "markdown", "markdown-inline", "python",
            "regex", "rust", "tsx", "typescript", "yaml",
        ];
        let mut names: Vec<&str> = curated().into_iter().map(|(n, _)| n).collect();
        let listed = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), listed, "a language is listed twice");
        want.sort_unstable();
        assert_eq!(names, want);
    }
}
