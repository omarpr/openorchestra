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

/// A clone with a bare origin — enough to exercise push against real git
/// without a network. Returns (origin, clone).
fn init_repo_with_origin(tmp: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let origin = tmp.join("origin.git");
    fs::create_dir_all(&origin).unwrap();
    git(&origin, &["init", "--bare", "-b", "main"]);

    let work = tmp.join("app");
    init_repo(&work);
    git(&work, &["remote", "add", "origin", origin.to_str().unwrap()]);
    git(&work, &["push", "-u", "origin", "main"]);
    (origin, work)
}

#[test]
fn push_state_counts_against_the_branchs_own_upstream() {
    let tmp = tempfile::tempdir().unwrap();
    let (_origin, work) = init_repo_with_origin(tmp.path());
    let g = GitWorktrees::new(SystemRunner);

    // Fully pushed: nothing to offer, and the upstream is known.
    let st = g.push_state(&work, "origin/main");
    assert_eq!(st.upstream.as_deref(), Some("origin/main"));
    assert_eq!(st.unpushed, 0);

    fs::write(work.join("a.txt"), "a\n").unwrap();
    git(&work, &["add", "."]);
    git(&work, &["commit", "-m", "one"]);
    assert_eq!(g.push_state(&work, "origin/main").unpushed, 1);

    // Push, and the offer goes away.
    g.push(&work, None).unwrap();
    assert_eq!(g.push_state(&work, "origin/main").unpushed, 0);
}

#[test]
fn a_never_published_branch_reports_no_upstream_and_publishes_with_u() {
    let tmp = tempfile::tempdir().unwrap();
    let (_origin, work) = init_repo_with_origin(tmp.path());
    let g = GitWorktrees::new(SystemRunner);

    let wt = tmp.path().join("wt");
    g.add_worktree(&work, &wt, "feat/push", None).unwrap();
    fs::write(wt.join("b.txt"), "b\n").unwrap();
    git(&wt, &["add", "."]);
    git(&wt, &["commit", "-m", "two"]);

    // No upstream yet, so the count falls back to "what publishing would send".
    let st = g.push_state(&wt, "origin/main");
    assert_eq!(st.upstream, None, "an unpublished branch has no tracking ref");
    assert_eq!(st.unpushed, 1);
    assert_eq!(g.default_remote(&wt).as_deref(), Some("origin"));

    g.push(&wt, Some(("origin", "feat/push"))).unwrap();

    // Publishing must also *record* the upstream — otherwise the next push
    // would still look like a first publication.
    let after = g.push_state(&wt, "origin/main");
    assert_eq!(after.upstream.as_deref(), Some("origin/feat/push"));
    assert_eq!(after.unpushed, 0);
}

#[test]
fn push_state_is_zero_in_a_repo_with_no_remote() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_root = tmp.path().join("solo");
    init_repo(&repo_root);
    let g = GitWorktrees::new(SystemRunner);

    // `fallback` cannot resolve either, so nothing is offered rather than an
    // error surfacing in the header.
    let st = g.push_state(&repo_root, "origin/main");
    assert_eq!(st, worktrees::PushState::default());
    assert_eq!(g.default_remote(&repo_root), None);
}

#[test]
fn a_worktree_branched_off_a_remote_ref_does_not_inherit_its_upstream() {
    // The production path: `add_worktree` is called with the repo's compare ref
    // (`origin/<default>`) as the base. Git's default `branch.autoSetupMerge`
    // would make the new branch track *that*, so `feat/x` would be born
    // claiming `origin/main` as its upstream — and pushing it would either fail
    // or land the task's commits on main.
    let tmp = tempfile::tempdir().unwrap();
    let (_origin, work) = init_repo_with_origin(tmp.path());
    let g = GitWorktrees::new(SystemRunner);

    let wt = tmp.path().join("wt");
    g.add_worktree(&work, &wt, "feat/from-remote", Some("origin/main")).unwrap();

    assert_eq!(
        git_out(&wt, &["config", "--get", "branch.feat/from-remote.merge"]).trim(),
        "",
        "the branch must not have inherited a tracking ref"
    );
    let st = g.push_state(&wt, "origin/main");
    assert_eq!(st.upstream, None, "nothing published yet");

    // And publishing still records the branch's own upstream.
    fs::write(wt.join("c.txt"), "c\n").unwrap();
    git(&wt, &["add", "."]);
    git(&wt, &["commit", "-m", "three"]);
    g.push(&wt, Some(("origin", "feat/from-remote"))).unwrap();
    let after = g.push_state(&wt, "origin/main");
    assert_eq!(after.upstream.as_deref(), Some("origin/feat/from-remote"));
    assert_eq!(after.unpushed, 0);
}

#[test]
fn an_upstream_pointing_at_another_branch_is_not_treated_as_published() {
    // Worktrees created before `--no-track` carry this state on disk, so the
    // fix cannot live at creation time alone. `@{upstream}` resolves happily to
    // `origin/main`; reporting it as published sends a bare `git push`, which
    // fails under `push.default = simple` and would otherwise push this
    // branch's commits onto main.
    let tmp = tempfile::tempdir().unwrap();
    let (_origin, work) = init_repo_with_origin(tmp.path());
    let g = GitWorktrees::new(SystemRunner);

    let wt = tmp.path().join("legacy");
    g.add_worktree(&work, &wt, "feat/legacy", Some("origin/main")).unwrap();
    git(&wt, &["config", "branch.feat/legacy.remote", "origin"]);
    git(&wt, &["config", "branch.feat/legacy.merge", "refs/heads/main"]);

    // Precondition: git really does resolve an upstream here.
    assert_eq!(
        git_out(&wt, &["rev-parse", "--abbrev-ref", "@{upstream}"]).trim(),
        "origin/main",
        "test setup should reproduce the inherited-upstream state"
    );

    fs::write(wt.join("d.txt"), "d\n").unwrap();
    git(&wt, &["add", "."]);
    git(&wt, &["commit", "-m", "four"]);

    let st = g.push_state(&wt, "origin/main");
    assert_eq!(
        st.upstream, None,
        "an upstream naming a different branch must route to publish, not push"
    );
    assert_eq!(st.unpushed, 1, "counted against the fallback instead");
}

#[test]
fn untracked_files_appear_in_the_diff_without_being_staged() {
    // The default view compares HEAD to the working tree, and a file you just
    // created is part of the working tree. `git diff` alone never lists it, so
    // the panel would omit brand new files exactly when you want them.
    let tmp = tempfile::tempdir().unwrap();
    let repo_root = tmp.path().join("app");
    init_repo(&repo_root);
    let g = GitWorktrees::new(SystemRunner);

    fs::write(repo_root.join("README.md"), "base\nedited\n").unwrap();
    fs::write(repo_root.join("brand-new.rs"), "fn main() {}\nfn other() {}\n").unwrap();
    fs::write(repo_root.join(".gitignore"), "ignored.log\n").unwrap();
    fs::write(repo_root.join("ignored.log"), "noise\n").unwrap();
    git(&repo_root, &["add", ".gitignore"]);
    git(&repo_root, &["commit", "-m", "ignore"]);

    let diff = g.diff_including_untracked(&repo_root, "HEAD").unwrap();
    let paths: Vec<&str> = diff.files.iter().map(|f| f.path.as_str()).collect();
    assert!(paths.contains(&"README.md"), "tracked edit still listed: {paths:?}");
    assert!(paths.contains(&"brand-new.rs"), "untracked file listed: {paths:?}");
    assert!(!paths.contains(&"ignored.log"), ".gitignore respected: {paths:?}");

    let new_file = diff.files.iter().find(|f| f.path == "brand-new.rs").unwrap();
    assert_eq!(new_file.change, Change::Added);
    assert_eq!(new_file.adds, 2, "both lines counted");
    assert_eq!(new_file.dels, 0);
    assert!(diff.unified.contains("brand-new.rs"), "content reaches the scrollable view");

    // Looking at a diff must not stage anything: `git add -N` would have made
    // this work for free and left the user's index modified.
    assert_eq!(
        git_out(&repo_root, &["status", "--porcelain", "brand-new.rs"]).trim(),
        "?? brand-new.rs",
        "the file must still be untracked afterwards"
    );
}

#[test]
fn a_flood_of_untracked_files_is_capped_and_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_root = tmp.path().join("app");
    init_repo(&repo_root);
    let g = GitWorktrees::new(SystemRunner);

    let extra = 5;
    for i in 0..worktrees::MAX_UNTRACKED_FILES + extra {
        fs::write(repo_root.join(format!("f{i:04}.txt")), "x\n").unwrap();
    }

    let diff = g.diff_including_untracked(&repo_root, "HEAD").unwrap();
    assert_eq!(diff.files.len(), worktrees::MAX_UNTRACKED_FILES, "capped");
    assert_eq!(diff.untracked_elided, extra, "and says how many it left out");
}

#[test]
fn a_clean_worktree_has_nothing_elided() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_root = tmp.path().join("app");
    init_repo(&repo_root);
    let g = GitWorktrees::new(SystemRunner);

    let diff = g.diff_including_untracked(&repo_root, "HEAD").unwrap();
    assert!(diff.files.is_empty());
    assert_eq!(diff.untracked_elided, 0);
}
