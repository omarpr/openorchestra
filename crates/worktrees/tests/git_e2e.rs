//! End-to-end lifecycle tests against **real git** in temp repos (architecture
//! §6.1). Unlike Spike 2's gh (not installed in CI), git is available, so these
//! drive actual `git worktree add/remove`, status, and merge-base diff.

use std::fs;
use std::path::Path;
use std::process::Command;

use worktrees::{
    create_task, read_task_brief, write_task_brief, Change, CreateRequest, CreateTaskMember,
    CreateTaskRequest, GitWorktrees, Manager, Registry, SystemRunner, TaskStore,
};

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
fn reads_the_checked_out_branch_before_the_first_commit() {
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-b", "main"]);
    let worktrees = GitWorktrees::new(SystemRunner);
    assert_eq!(worktrees.current_branch(repo.path()).unwrap(), "main");

    git(repo.path(), &["commit", "--allow-empty", "-m", "first commit"]);
    git(repo.path(), &["checkout", "--detach"]);
    assert_eq!(worktrees.current_branch(repo.path()).unwrap(), "HEAD");
}

#[test]
fn projects_can_create_worktrees_with_the_same_name_and_branch() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let registry = Registry::new_at(root.join("registry.json"));
    let mut manager = Manager::new(registry, GitWorktrees::new(SystemRunner), root.join("worktrees"));
    let mut tasks = TaskStore::new_at(root.join("tasks.json"));
    let tasks_root = root.join("tasks");

    for (project, expected) in [
        ("Web App", "settings-web-app-feat-settings"),
        ("API", "settings-api-feat-settings"),
        ("Web-App", "settings-web-app-feat-settings-2"),
    ] {
        let repo = root.join(project);
        init_repo(&repo);
        let id = create_task(
            &mut manager,
            &mut tasks,
            CreateTaskRequest {
                name: "Settings".into(),
                slug: "settings".into(),
                home_repo: project.into(),
                members: vec![CreateTaskMember {
                    repo: project.into(),
                    repo_root: repo,
                    branch: "feat/settings".into(),
                    base_ref: Some("main".into()),
                }],
                created_at: "2026-11-18T00:00:00Z".into(),
                agent_kind: Default::default(),
            },
        )
        .unwrap();
        assert_eq!(id, expected);
        let task = tasks.get(&id).unwrap();
        assert_eq!(task.name, "Settings");
        assert_eq!(task.slug, id);
        let worktree = &task.members[0];
        assert_eq!(worktree.path, root.join("worktrees").join(project).join(&id));
        assert_eq!(git_out(&worktree.path, &["branch", "--show-current"]).trim(), "feat/settings");
        write_task_brief(&tasks_root, &task.slug, project).unwrap();
    }

    let tasks = TaskStore::load(root.join("tasks.json")).unwrap();
    for task in tasks.tasks() {
        assert_eq!(read_task_brief(&tasks_root, &task.slug).as_deref(), Some(task.home_repo.as_str()));
    }
}

#[test]
fn task_with_multiple_projects_uses_the_home_projects_branch_in_its_slug() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let mut manager = Manager::new(
        Registry::new_at(root.join("registry.json")),
        GitWorktrees::new(SystemRunner),
        root.join("worktrees"),
    );
    let mut tasks = TaskStore::new_at(root.join("tasks.json"));
    let members = [("frontend", "feature/settings-ui"), ("backend", "bugfix/settings")]
        .into_iter()
        .map(|(project, branch)| {
            let repo_root = root.join(project);
            init_repo(&repo_root);
            CreateTaskMember {
                repo: project.into(),
                repo_root,
                branch: branch.into(),
                base_ref: Some("main".into()),
            }
        })
        .collect();
    let id = create_task(
        &mut manager,
        &mut tasks,
        CreateTaskRequest {
            name: "Settings".into(),
            slug: "settings".into(),
            home_repo: "backend".into(),
            members,
            created_at: "2026-11-18T00:00:00Z".into(),
            agent_kind: Default::default(),
        },
    )
    .unwrap();

    assert_eq!(id, "settings-backend-bugfix-settings");
    let task = tasks.get(&id).unwrap();
    assert_eq!(task.members.len(), 2);
    for member in &task.members {
        let entry = manager.registry.get(&member.worktree_id).unwrap();
        assert_eq!(entry.task_id.as_deref(), Some(id.as_str()));
        assert_eq!(entry.task_slug, id);
        assert_eq!(git_out(&member.path, &["branch", "--show-current"]).trim(), member.branch);
    }
}

#[test]
fn task_creation_rolls_back_after_a_later_member_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let a = root.join("a");
    let b = root.join("b");
    init_repo(&a);
    init_repo(&b);
    let registry_path = root.join("registry.json");
    let mut manager = Manager::new(
        Registry::new_at(&registry_path),
        GitWorktrees::new(SystemRunner),
        root.join("worktrees"),
    );
    let mut tasks = TaskStore::new_at(root.join("tasks.json"));
    let mut request = CreateTaskRequest {
        name: "Settings".into(),
        slug: "settings".into(),
        home_repo: "a".into(),
        members: [("a", &a, "main"), ("b", &b, "missing-base")]
            .into_iter()
            .map(|(repo, path, base)| CreateTaskMember {
                repo: repo.into(),
                repo_root: path.clone(),
                branch: "feat/settings".into(),
                base_ref: Some(base.into()),
            })
            .collect(),
        created_at: String::new(),
        agent_kind: Default::default(),
    };

    git(&b, &["branch", "feat/settings"]);
    assert!(create_task(&mut manager, &mut tasks, request.clone())
        .unwrap_err().to_string().contains("already exists"));
    assert!(!root.join("worktrees/a").exists());
    git(&b, &["branch", "-D", "feat/settings"]);

    assert!(create_task(&mut manager, &mut tasks, request.clone()).is_err());
    assert!(manager.registry.entries().is_empty());
    assert!(Registry::load(registry_path).unwrap().entries().is_empty());
    assert!(tasks.tasks().is_empty());
    assert!(!manager.git().ref_exists(&a, "refs/heads/feat/settings"));
    assert_eq!(manager.git().list_worktrees(&a).unwrap().len(), 1);

    request.members[1].base_ref = Some("main".into());
    assert!(create_task(&mut manager, &mut tasks, request).is_ok());
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
    // The summary carries no bodies; a file's diff is fetched when asked for,
    // against the base the summary already pinned.
    let body = mgr.git().file_diff(&nav_path, &diff.base, "new.rs", false, None);
    assert!(body.contains("new.rs"), "the file's body is fetched on demand");
    assert!(
        !body.contains("README.md"),
        "a per-file fetch must not drag in the rest of the changeset"
    );

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

#[test]
fn concurrent_adapter_excludes_merge_in_the_common_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("app");
    init_repo(&repo);
    let git = GitWorktrees::new(SystemRunner);
    let worktree = tmp.path().join("linked");
    git.add_worktree(&repo, &worktree, "feat/linked", None).unwrap();
    let start = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            start.wait();
            git.exclude_locally(&repo, &[".harness/", ".codex/"]).unwrap();
        });
        scope.spawn(|| {
            start.wait();
            git.exclude_locally(&worktree, &[".harness/", ".claude/settings.local.json"]).unwrap();
        });
    });
    let exclude = fs::read_to_string(repo.join(".git/info/exclude")).unwrap();
    assert_eq!(exclude.matches(".harness/").count(), 1);
    assert!(exclude.contains(".codex/"));
    assert!(exclude.contains(".claude/settings.local.json"));
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
    assert!(
        new_file.untracked,
        "flagged untracked, so its body is fetched with --no-index"
    );
    let body = g.file_diff(&repo_root, &diff.base, "brand-new.rs", true, None);
    assert!(body.contains("brand-new.rs"), "content reaches the scrollable view");

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

#[test]
fn a_renamed_files_body_is_fetched_as_a_rename_not_as_a_fresh_file() {
    // Bodies are fetched one path at a time now. `git diff <base> -- <new>`
    // alone cannot see a rename — nothing existed under that name at the base —
    // so the whole file would come back as an addition. The summary carries the
    // old path precisely so the fetch can name both ends.
    let tmp = tempfile::tempdir().unwrap();
    let repo_root = tmp.path().join("app");
    init_repo(&repo_root);
    // Enough content that git scores the pair as a rename rather than an
    // unrelated add/delete.
    let body: String = (0..40).map(|i| format!("line {i}\n")).collect();
    fs::write(repo_root.join("old.rs"), &body).unwrap();
    git(&repo_root, &["add", "."]);
    git(&repo_root, &["commit", "-m", "add old.rs"]);

    let g = GitWorktrees::new(SystemRunner);
    let base = git_out(&repo_root, &["rev-parse", "HEAD"]).trim().to_string();

    git(&repo_root, &["mv", "old.rs", "new.rs"]);
    fs::write(repo_root.join("new.rs"), format!("{body}line 40\n")).unwrap();

    let diff = g.diff(&repo_root, "HEAD").unwrap();
    let renamed = diff
        .files
        .iter()
        .find(|f| f.path == "new.rs")
        .expect("the renamed file is listed under its new path");
    assert_eq!(renamed.change, Change::Renamed);
    assert_eq!(
        renamed.renamed_from.as_deref(),
        Some("old.rs"),
        "the summary records where it came from"
    );

    let with_old = g.file_diff(&repo_root, &base, "new.rs", false, renamed.renamed_from.as_deref());
    assert!(
        with_old.contains("rename from old.rs") && with_old.contains("rename to new.rs"),
        "naming both ends yields a rename, got:\n{with_old}"
    );

    // The regression this guards: drop the old path and the same change reads
    // as forty new lines out of nowhere.
    let without_old = g.file_diff(&repo_root, &base, "new.rs", false, None);
    assert!(
        !without_old.contains("rename from"),
        "precondition: a single-path fetch cannot detect the rename"
    );
}

/// A repo's `.git/config` can name commands git runs on its own. The app's
/// background queries must not run them.
#[test]
fn background_queries_do_not_run_config_commands() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("repo");
    init_repo(&root);
    let marker = tmp.path().join("ran");
    let hook = tmp.path().join("fsmonitor.sh");
    fs::write(&hook, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
    git(&root, &["config", "core.fsmonitor", &hook.to_string_lossy()]);
    git(&root, &["config", "diff.external", &hook.to_string_lossy()]);
    fs::write(root.join("README.md"), "changed\n").unwrap();

    let g = GitWorktrees::new(SystemRunner);
    let status = g.status(&root, None).unwrap();
    assert_eq!(status.dirty_files, 1);
    let _ = g.diff(&root, "main");
    let _ = g.file_diff(&root, "main", "README.md", false, None);
    assert!(!marker.exists(), "a config-named command ran during a background query");
}

/// A remote named like an option must never reach `git push` as a flag.
#[test]
fn a_remote_named_like_an_option_is_never_offered_or_pushed_to() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("repo");
    init_repo(&root);
    let marker = tmp.path().join("pwned");
    let bare = tmp.path().join("bare.git");
    git(tmp.path(), &["init", "--bare", &bare.to_string_lossy()]);
    // `git remote add` refuses dash names; a hand-edited config does not.
    let cfg = root.join(".git/config");
    let mut text = fs::read_to_string(&cfg).unwrap();
    text.push_str(&format!(
        "[remote \"--receive-pack=touch {}\"]\n\turl = {}\n",
        marker.display(),
        bare.display()
    ));
    fs::write(&cfg, text).unwrap();

    let g = GitWorktrees::new(SystemRunner);
    assert_eq!(g.default_remote(&root), None, "a dash-named remote must not be offered");
    let name = format!("--receive-pack=touch {}", marker.display());
    assert!(g.push(&root, Some((&name, "main"))).is_err());
    assert!(!marker.exists());
}

#[test]
fn refs_that_look_like_options_are_rejected() {
    assert!(worktrees::is_safe_ref("origin/main"));
    assert!(!worktrees::is_safe_ref("--output=/tmp/x"));
    assert!(!worktrees::is_safe_ref(""));
}
