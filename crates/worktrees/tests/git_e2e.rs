//! End-to-end lifecycle tests against **real git** in temp repos (architecture
//! §6.1). Unlike Spike 2's gh (not installed in CI), git is available, so these
//! drive actual `git worktree add/remove`, status, and merge-base diff.

use std::fs;
use std::path::Path;
use std::process::Command;

use worktrees::{Change, CreateRequest, GitWorktrees, Manager, Registry, SystemRunner};

/// Run git for test setup, asserting success.
fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e")
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn init_repo(root: &Path) {
    fs::create_dir_all(root).unwrap();
    git(root, &["init", "-b", "main"]);
    git(root, &["config", "user.email", "t@e"]);
    git(root, &["config", "user.name", "t"]);
    fs::write(root.join("README.md"), "base\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "base"]);
}

#[test]
fn full_lifecycle_create_status_diff_archive_delete() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_root = tmp.path().join("app");
    init_repo(&repo_root);

    let wt_root = tmp.path().join("worktrees");
    let registry = Registry::load(tmp.path().join("registry.json")).unwrap();
    let mut mgr = Manager::new(registry, GitWorktrees::new(SystemRunner), &wt_root);

    // ---- create two worktrees ----
    let nav = mgr
        .create(CreateRequest {
            repo: "app".into(),
            repo_root: repo_root.clone(),
            task_slug: "nav".into(),
            branch: "feat/nav".into(),
            base_ref: None,
            created_at: "2026-07-19T00:00:00Z".into(),
            agent_kind: Default::default(),
        })
        .unwrap();
    let diff_id = mgr
        .create(CreateRequest {
            repo: "app".into(),
            repo_root: repo_root.clone(),
            task_slug: "diff".into(),
            branch: "feat/diff".into(),
            base_ref: None,
            created_at: "2026-07-19T00:00:00Z".into(),
            agent_kind: Default::default(),
        })
        .unwrap();

    let nav_path = wt_root.join("app").join("nav");
    let diff_path = wt_root.join("app").join("diff");
    assert!(nav_path.join("README.md").exists(), "worktree checked out");
    assert_eq!(mgr.registry.active().count(), 2);
    assert_eq!(mgr.git().default_branch(&repo_root).unwrap(), "main");

    // main + 2 worktrees.
    assert_eq!(mgr.git().list_worktrees(&repo_root).unwrap().len(), 3);

    // ---- make changes in the nav worktree ----
    // one committed add + one uncommitted modification.
    fs::write(nav_path.join("new.rs"), "fn main() {}\n").unwrap();
    git(&nav_path, &["add", "new.rs"]);
    git(&nav_path, &["commit", "-m", "add new.rs"]);
    fs::write(nav_path.join("README.md"), "base\nnav change\n").unwrap();

    // ---- status ----
    let st = mgr.git().status(&nav_path, Some("main")).unwrap();
    assert_eq!(st.ahead, 1, "one commit beyond main");
    assert_eq!(st.behind, 0);
    assert_eq!(st.dirty_files, 1, "README modified, uncommitted");

    // ---- diff against merge-base(main, HEAD) ----
    let diff = mgr.git().diff(&nav_path, "main").unwrap();
    let by_path = |p: &str| diff.files.iter().find(|f| f.path == p).cloned();
    let readme = by_path("README.md").expect("README in diff");
    let newf = by_path("new.rs").expect("new.rs in diff");
    assert_eq!(readme.change, Change::Modified);
    assert_eq!(newf.change, Change::Added);
    assert!(newf.adds >= 1);
    let (adds, _dels) = diff.totals();
    assert!(adds >= 2, "committed add + uncommitted line");
    assert!(diff.unified.contains("new.rs"), "unified text present");

    // ---- archive nav: dir gone, branch + entry kept ----
    mgr.archive(&nav).unwrap();
    assert!(!nav_path.exists(), "archived worktree dir removed");
    assert!(mgr.registry.get(&nav).unwrap().archived);
    assert!(
        git_out(&repo_root, &["branch", "--list", "feat/nav"]).contains("feat/nav"),
        "archive keeps the branch"
    );

    // ---- delete diff: dir, branch, and entry all gone ----
    mgr.delete(&diff_id).unwrap();
    assert!(!diff_path.exists(), "deleted worktree dir removed");
    assert!(mgr.registry.get(&diff_id).is_none());
    assert!(
        git_out(&repo_root, &["branch", "--list", "feat/diff"])
            .trim()
            .is_empty(),
        "delete removes the branch"
    );

    // ---- persisted across reload ----
    let reloaded = Registry::load(tmp.path().join("registry.json")).unwrap();
    assert!(reloaded.get(&nav).unwrap().archived);
    assert!(reloaded.get(&diff_id).is_none());
}

#[test]
fn diff_is_empty_when_base_is_unresolvable() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_root = tmp.path().join("app");
    init_repo(&repo_root);

    let git = GitWorktrees::new(SystemRunner);
    // No `origin` remote in a fresh temp repo, so merge-base(origin/main, HEAD)
    // cannot resolve -> empty diff, not an error (graceful degradation).
    let diff = git.diff(&repo_root, "origin/main").unwrap();
    assert!(diff.files.is_empty());
    assert!(diff.base.is_empty());
}

// ---- local excludes (harness must not dirty the user's repo) ---------------

#[test]
fn exclude_locally_hides_harness_files_in_a_real_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_root = tmp.path().join("app");
    init_repo(&repo_root);
    let g = GitWorktrees::new(SystemRunner);
    let wt = tmp.path().join("wt");
    g.add_worktree(&repo_root, &wt, "feat/excl", None).unwrap();

    // Exactly what the adapters drop into a worktree.
    fs::create_dir_all(wt.join(".harness")).unwrap();
    fs::write(wt.join(".harness/status.json"), "{}").unwrap();
    fs::create_dir_all(wt.join(".codex")).unwrap();
    fs::write(wt.join(".codex/config.toml"), "notify = []").unwrap();
    fs::create_dir_all(wt.join(".claude")).unwrap();
    fs::write(wt.join(".claude/settings.local.json"), "{}").unwrap();

    assert!(
        !git_out(&wt, &["status", "--porcelain"]).trim().is_empty(),
        "precondition: harness files show as changes before excluding"
    );

    g.exclude_locally(&wt, &[".harness/", ".codex/", ".claude/settings.local.json"])
        .unwrap();

    assert_eq!(
        git_out(&wt, &["status", "--porcelain"]).trim(),
        "",
        "harness-written files must not appear as changes in the user's repo"
    );
}

#[test]
fn exclude_locally_is_idempotent_and_preserves_the_users_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_root = tmp.path().join("app");
    init_repo(&repo_root);
    let g = GitWorktrees::new(SystemRunner);

    let exclude = repo_root.join(".git/info/exclude");
    fs::create_dir_all(exclude.parent().unwrap()).unwrap();
    fs::write(&exclude, "# the user's own entry\nmy-scratch/\n").unwrap();

    g.exclude_locally(&repo_root, &[".harness/"]).unwrap();
    g.exclude_locally(&repo_root, &[".harness/", ".codex/"]).unwrap();
    g.exclude_locally(&repo_root, &[".harness/", ".codex/"]).unwrap();

    let text = fs::read_to_string(&exclude).unwrap();
    assert!(text.contains("my-scratch/"), "the user's entries must survive: {text}");
    assert_eq!(
        text.matches(".harness/").count(),
        1,
        "repeated calls must replace our block, not append forever: {text}"
    );
    assert!(text.contains(".codex/"));
}

#[test]
fn exclude_locally_targets_the_common_dir_not_the_worktree_git_dir() {
    // Git reads only $GIT_COMMON_DIR/info/exclude. A linked worktree's own
    // $GIT_DIR/info/exclude is silently ignored — verified against real git,
    // and getting it wrong yields code that looks correct and does nothing.
    let tmp = tempfile::tempdir().unwrap();
    let repo_root = tmp.path().join("app");
    init_repo(&repo_root);
    let g = GitWorktrees::new(SystemRunner);
    let wt = tmp.path().join("wt2");
    g.add_worktree(&repo_root, &wt, "feat/excl2", None).unwrap();

    g.exclude_locally(&wt, &[".harness/"]).unwrap();

    let common = repo_root.join(".git/info/exclude");
    assert!(common.exists(), "must write the common dir's exclude file");
    assert!(fs::read_to_string(&common).unwrap().contains(".harness/"));
}
