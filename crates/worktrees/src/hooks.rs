//! Hybrid worktree setup/teardown hooks (committed + local override).
//!
//! Committed defaults live at `<repo_root>/.openorchestra/hooks.toml`.
//! Machine-local overrides live at `<data_dir>/hooks/<repo>.toml`.
//! When local sets `scripts`, that list **replaces** the committed list.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::runner::CommandRunner;

/// One phase of the hook lifecycle.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookPhase {
    #[serde(default)]
    pub scripts: Vec<String>,
}

/// Parsed hooks.toml contents.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HooksFile {
    #[serde(default)]
    pub setup: HookPhase,
    #[serde(default)]
    pub teardown: HookPhase,
}

/// Effective hooks after merge, with scripts resolved to absolute paths
/// relative to the main checkout (`repo_root`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HooksConfig {
    pub setup: Vec<PathBuf>,
    pub teardown: Vec<PathBuf>,
}

/// Context passed into hook scripts via the environment.
#[derive(Debug, Clone)]
pub struct HookEnv {
    pub repo_root: PathBuf,
    pub worktree: PathBuf,
    pub branch: String,
    pub task_slug: String,
    pub task_id: Option<String>,
    pub home_worktree: Option<PathBuf>,
}

impl HookEnv {
    pub fn to_map(&self) -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert(
            "HARNESS_REPO_ROOT".into(),
            self.repo_root.to_string_lossy().into_owned(),
        );
        m.insert(
            "HARNESS_WORKTREE".into(),
            self.worktree.to_string_lossy().into_owned(),
        );
        m.insert("HARNESS_BRANCH".into(), self.branch.clone());
        m.insert("HARNESS_TASK_SLUG".into(), self.task_slug.clone());
        if let Some(id) = &self.task_id {
            m.insert("HARNESS_TASK_ID".into(), id.clone());
        }
        if let Some(home) = &self.home_worktree {
            m.insert(
                "HARNESS_HOME_WORKTREE".into(),
                home.to_string_lossy().into_owned(),
            );
        }
        m
    }
}

#[derive(Debug)]
pub enum HooksError {
    Io(std::io::Error),
    Parse(String),
    ScriptFailed { script: PathBuf, status: Option<i32>, stderr: String },
}

impl std::fmt::Display for HooksError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HooksError::Io(e) => write!(f, "{e}"),
            HooksError::Parse(e) => write!(f, "hooks parse error: {e}"),
            HooksError::ScriptFailed {
                script,
                status,
                stderr,
            } => write!(
                f,
                "hook {} failed (status {:?}): {stderr}",
                script.display(),
                status
            ),
        }
    }
}

impl std::error::Error for HooksError {}

impl From<std::io::Error> for HooksError {
    fn from(e: std::io::Error) -> Self {
        HooksError::Io(e)
    }
}

/// Path to the committed hooks file inside a repo.
pub fn committed_hooks_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".openorchestra").join("hooks.toml")
}

/// Path to the local override for a repo display name.
pub fn local_hooks_path(data_dir: &Path, repo: &str) -> PathBuf {
    data_dir.join("hooks").join(format!("{repo}.toml"))
}

fn load_file(path: &Path) -> Result<Option<HooksFile>, HooksError> {
    match fs::read_to_string(path) {
        Ok(text) => {
            let file: HooksFile = toml::from_str(&text)
                .map_err(|e| HooksError::Parse(format!("{path:?}: {e}")))?;
            Ok(Some(file))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(HooksError::Io(e)),
    }
}

fn resolve_scripts(repo_root: &Path, scripts: &[String]) -> Vec<PathBuf> {
    scripts
        .iter()
        .map(|s| {
            let p = Path::new(s);
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                repo_root.join(p)
            }
        })
        .collect()
}

/// Prefer local phases entirely when a local file exists (replace, not append).
pub fn merge_hooks_replace(committed: &HooksFile, local: Option<&HooksFile>) -> HooksFile {
    match local {
        None => committed.clone(),
        Some(local) => HooksFile {
            setup: local.setup.clone(),
            teardown: local.teardown.clone(),
        },
    }
}

impl HooksConfig {
    /// Load committed + optional local override and resolve script paths.
    pub fn load(repo_root: &Path, data_dir: &Path, repo: &str) -> Result<Self, HooksError> {
        let committed = load_file(&committed_hooks_path(repo_root))?
            .unwrap_or_default();
        let local = load_file(&local_hooks_path(data_dir, repo))?;
        let merged = merge_hooks_replace(&committed, local.as_ref());
        Ok(HooksConfig {
            setup: resolve_scripts(repo_root, &merged.setup.scripts),
            teardown: resolve_scripts(repo_root, &merged.teardown.scripts),
        })
    }

    /// Shell command lines to run setup scripts (cwd should be the worktree).
    /// Suitable for sending into a visible terminal.
    pub fn setup_shell_commands(&self, env: &HookEnv) -> Vec<String> {
        shell_commands_for(&self.setup, env)
    }

    pub fn teardown_shell_commands(&self, env: &HookEnv) -> Vec<String> {
        shell_commands_for(&self.teardown, env)
    }

    /// Run setup scripts with `cwd = worktree` via the command runner.
    pub fn run_setup<R: CommandRunner>(
        &self,
        runner: &R,
        env: &HookEnv,
    ) -> Result<(), HooksError> {
        run_scripts(runner, &self.setup, env)
    }

    pub fn run_teardown<R: CommandRunner>(
        &self,
        runner: &R,
        env: &HookEnv,
    ) -> Result<(), HooksError> {
        run_scripts(runner, &self.teardown, env)
    }
}

fn shell_commands_for(scripts: &[PathBuf], env: &HookEnv) -> Vec<String> {
    let exports: String = env
        .to_map()
        .iter()
        .map(|(k, v)| format!("export {k}={}", shell_quote(v)))
        .collect::<Vec<_>>()
        .join("; ");
    scripts
        .iter()
        .map(|script| {
            let run = format!("{} {}", shell_quote(&script.to_string_lossy()), "");
            if exports.is_empty() {
                format!("{run};")
            } else {
                format!("{exports}; {run}")
            }
        })
        .collect()
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn run_scripts<R: CommandRunner>(
    runner: &R,
    scripts: &[PathBuf],
    env: &HookEnv,
) -> Result<(), HooksError> {
    for script in scripts {
        // Pass env by prefixing a shell invocation so CommandRunner stays simple.
        let mut exports = String::new();
        for (k, v) in env.to_map() {
            exports.push_str(&format!("export {k}={}; ", shell_quote(&v)));
        }
        let cmdline = format!(
            "{exports}exec {}",
            shell_quote(&script.to_string_lossy())
        );
        let out = runner.run(
            Some(&env.worktree),
            "sh",
            &["-lc", &cmdline],
        )?;
        if !out.success() {
            return Err(HooksError::ScriptFailed {
                script: script.clone(),
                status: out.status,
                stderr: out.stderr,
            });
        }
    }
    Ok(())
}

/// Load task-level scripts from `~/.openorchestra/tasks/<slug>/`.
/// Looks for `setup.sh` / `teardown.sh` (executable shell scripts).
#[derive(Debug, Clone, Default)]
pub struct TaskHooks {
    pub setup: Option<PathBuf>,
    pub teardown: Option<PathBuf>,
}

impl TaskHooks {
    pub fn load(data_dir: &Path, task_slug: &str) -> Self {
        let dir = data_dir.join("tasks").join(task_slug);
        let setup = dir.join("setup.sh");
        let teardown = dir.join("teardown.sh");
        TaskHooks {
            setup: setup.is_file().then_some(setup),
            teardown: teardown.is_file().then_some(teardown),
        }
    }

    pub fn setup_shell_command(&self, member_paths: &[(String, PathBuf)]) -> Option<String> {
        let script = self.setup.as_ref()?;
        Some(task_hook_command(script, member_paths))
    }

    pub fn teardown_shell_command(&self, member_paths: &[(String, PathBuf)]) -> Option<String> {
        let script = self.teardown.as_ref()?;
        Some(task_hook_command(script, member_paths))
    }

    pub fn run_setup<R: CommandRunner>(
        &self,
        runner: &R,
        cwd: &Path,
        member_paths: &[(String, PathBuf)],
    ) -> Result<(), HooksError> {
        let Some(script) = &self.setup else {
            return Ok(());
        };
        run_task_script(runner, script, cwd, member_paths)
    }

    pub fn run_teardown<R: CommandRunner>(
        &self,
        runner: &R,
        cwd: &Path,
        member_paths: &[(String, PathBuf)],
    ) -> Result<(), HooksError> {
        let Some(script) = &self.teardown else {
            return Ok(());
        };
        run_task_script(runner, script, cwd, member_paths)
    }
}

fn task_hook_command(script: &Path, member_paths: &[(String, PathBuf)]) -> String {
    let mut exports = String::new();
    for (repo, path) in member_paths {
        let key = format!(
            "HARNESS_MEMBER_{}",
            repo.to_uppercase().replace('-', "_")
        );
        exports.push_str(&format!(
            "export {key}={}; ",
            shell_quote(&path.to_string_lossy())
        ));
    }
    format!(
        "{exports}{}",
        shell_quote(&script.to_string_lossy())
    )
}

fn run_task_script<R: CommandRunner>(
    runner: &R,
    script: &Path,
    cwd: &Path,
    member_paths: &[(String, PathBuf)],
) -> Result<(), HooksError> {
    let cmdline = task_hook_command(script, member_paths);
    let out = runner.run(Some(cwd), "sh", &["-lc", &cmdline])?;
    if !out.success() {
        return Err(HooksError::ScriptFailed {
            script: script.to_path_buf(),
            status: out.status,
            stderr: out.stderr,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn merge_replace_uses_local_when_present() {
        let committed = HooksFile {
            setup: HookPhase {
                scripts: vec!["a.sh".into()],
            },
            teardown: HookPhase {
                scripts: vec!["t.sh".into()],
            },
        };
        let local = HooksFile {
            setup: HookPhase {
                scripts: vec!["local-setup.sh".into()],
            },
            teardown: HookPhase { scripts: vec![] },
        };
        let merged = merge_hooks_replace(&committed, Some(&local));
        assert_eq!(merged.setup.scripts, vec!["local-setup.sh"]);
        assert!(merged.teardown.scripts.is_empty());
    }

    #[test]
    fn merge_replace_keeps_committed_when_no_local() {
        let committed = HooksFile {
            setup: HookPhase {
                scripts: vec!["a.sh".into()],
            },
            teardown: HookPhase::default(),
        };
        let merged = merge_hooks_replace(&committed, None);
        assert_eq!(merged.setup.scripts, vec!["a.sh"]);
    }

    #[test]
    fn load_resolves_relative_to_repo_root() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let data = tmp.path().join("data");
        fs::create_dir_all(repo.join(".openorchestra")).unwrap();
        let mut f = fs::File::create(committed_hooks_path(&repo)).unwrap();
        writeln!(
            f,
            "[setup]\nscripts = [\"scripts/setup.sh\"]\n[teardown]\nscripts = []"
        )
        .unwrap();

        let cfg = HooksConfig::load(&repo, &data, "repo").unwrap();
        assert_eq!(cfg.setup, vec![repo.join("scripts/setup.sh")]);
        assert!(cfg.teardown.is_empty());
    }

    #[test]
    fn local_override_replaces_committed() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let data = tmp.path().join("data");
        fs::create_dir_all(repo.join(".openorchestra")).unwrap();
        fs::create_dir_all(data.join("hooks")).unwrap();
        fs::write(
            committed_hooks_path(&repo),
            "[setup]\nscripts = [\"committed.sh\"]\n",
        )
        .unwrap();
        fs::write(
            local_hooks_path(&data, "myrepo"),
            "[setup]\nscripts = [\"local.sh\"]\n[teardown]\nscripts = []\n",
        )
        .unwrap();

        let cfg = HooksConfig::load(&repo, &data, "myrepo").unwrap();
        assert_eq!(cfg.setup, vec![repo.join("local.sh")]);
    }
}
