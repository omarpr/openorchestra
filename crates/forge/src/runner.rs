//! Command execution behind a trait so tests can inject recorded gh output.

use std::io;
use std::path::Path;

/// Captured output of a finished command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    /// Process exit code; `None` if terminated by a signal.
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl CommandOutput {
    pub fn success(&self) -> bool {
        self.status == Some(0)
    }
}

/// Runs a command and captures its output. [`SystemRunner`] is the real
/// implementation; tests substitute a recorder/playback fake.
pub trait CommandRunner: Send + Sync {
    /// Run `program` with `args`, optionally in working directory `cwd`.
    /// An `Err` means the command could not be *started* (e.g. binary missing);
    /// a command that started but exited non-zero is an `Ok` with its status.
    fn run(&self, cwd: Option<&Path>, program: &str, args: &[&str]) -> io::Result<CommandOutput>;
}

/// Executes commands via `std::process::Command`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn run(&self, cwd: Option<&Path>, program: &str, args: &[&str]) -> io::Result<CommandOutput> {
        let mut cmd = os_compat::process::command(program);
        cmd.args(args);
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }
        let out = cmd.output()?;
        Ok(CommandOutput {
            status: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}
