//! Registry unit tests — pure, no git (architecture §6.1).

use std::path::PathBuf;

use worktrees::{Avatar, Registry, RegistryError, WorktreeEntry};

fn entry(repo: &str, slug: &str) -> WorktreeEntry {
    WorktreeEntry {
        id: WorktreeEntry::id_for(repo, slug),
        repo: repo.to_string(),
        repo_root: PathBuf::from(format!("/repos/{repo}")),
        branch: format!("feat/{slug}"),
        task_slug: slug.to_string(),
        name: slug.to_string(),
        path: PathBuf::from(format!("/wt/{repo}/{slug}")),
        session_file: None,
        avatar: Avatar::from_slug(slug),
        pr_number: None,
        created_at: "2026-07-19T00:00:00Z".to_string(),
        archived: false,
    }
}

#[test]
fn add_get_and_reject_duplicates() {
    let mut reg = Registry::default();
    reg.add(entry("app", "nav")).unwrap();
    assert!(reg.get("app/nav").is_some());

    let dup = reg.add(entry("app", "nav"));
    assert_eq!(dup, Err(RegistryError::Duplicate("app/nav".to_string())));
    assert_eq!(reg.entries().len(), 1);
}

#[test]
fn archive_keeps_entry_delete_removes_it() {
    let mut reg = Registry::default();
    reg.add(entry("app", "nav")).unwrap();
    reg.add(entry("app", "diff")).unwrap();

    reg.mark_archived("app/nav").unwrap();
    assert!(reg.get("app/nav").unwrap().archived);
    assert_eq!(reg.active().count(), 1);
    assert_eq!(reg.archived().count(), 1);

    let removed = reg.remove("app/diff");
    assert!(removed.is_some());
    assert!(reg.get("app/diff").is_none());
}

#[test]
fn rename_and_avatar_are_registry_only() {
    let mut reg = Registry::default();
    reg.add(entry("app", "nav")).unwrap();

    reg.rename("app/nav", "Navigation rework").unwrap();
    assert_eq!(reg.get("app/nav").unwrap().name, "Navigation rework");
    // Branch (git state) is untouched by a rename.
    assert_eq!(reg.get("app/nav").unwrap().branch, "feat/nav");

    reg.set_avatar("app/nav", Avatar { initials: "NR".into(), color: 3 })
        .unwrap();
    assert_eq!(reg.get("app/nav").unwrap().avatar.color, 3);

    assert_eq!(
        reg.rename("app/missing", "x"),
        Err(RegistryError::NotFound("app/missing".to_string()))
    );
}

#[test]
fn grouped_by_repo_preserves_order_and_skips_archived() {
    let mut reg = Registry::default();
    reg.add(entry("app", "nav")).unwrap();
    reg.add(entry("lib", "parse")).unwrap();
    reg.add(entry("app", "diff")).unwrap();
    reg.mark_archived("lib/parse").unwrap();

    let groups = reg.grouped_by_repo();
    assert_eq!(groups.len(), 1, "archived lib/parse hides its only repo group");
    assert_eq!(groups[0].repo, "app");
    assert_eq!(groups[0].entries.len(), 2);
}

#[test]
fn persists_and_reloads_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("registry.json");

    let mut reg = Registry::load(&path).unwrap(); // missing -> empty
    assert!(reg.entries().is_empty());
    reg.add(entry("app", "nav")).unwrap();
    reg.set_pr_number("app/nav", Some(42)).unwrap();
    reg.save().unwrap();

    // No leftover temp file after an atomic save.
    assert!(!path.with_extension("json.tmp").exists());

    let reloaded = Registry::load(&path).unwrap();
    assert_eq!(reloaded.entries().len(), 1);
    assert_eq!(reloaded.get("app/nav").unwrap().pr_number, Some(42));
}

#[test]
fn avatar_initials_are_derived_and_stable() {
    assert_eq!(Avatar::from_slug("nav-rework").initials, "NR");
    assert_eq!(Avatar::from_slug("diff").initials, "DI");
    assert_eq!(Avatar::from_slug("fix_the_bug").initials, "FT");
    // Deterministic color for a given slug.
    assert_eq!(
        Avatar::from_slug("nav-rework").color,
        Avatar::from_slug("nav-rework").color
    );
}
