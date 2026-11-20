import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const cliConfig = "OPENCODE_CLI_CONFIG_CONTENT";
const shims = path.dirname(fileURLToPath(import.meta.url));

function configuredPlugins() {
  const root = process.env.XDG_CONFIG_HOME || path.join(os.homedir(), ".config");
  const directory = path.join(root, "opencode");
  try {
    return (JSON.parse(fs.readFileSync(path.join(directory, "cli.json"), "utf8")).plugins ?? []).map((entry) => {
      const spec = typeof entry === "string" ? entry : entry.package;
      if (!spec?.startsWith("./") && !spec?.startsWith("../")) return entry;
      const absolute = path.resolve(directory, spec);
      return typeof entry === "string" ? absolute : { ...entry, package: absolute };
    });
  } catch (error) {
    if (error.code === "ENOENT") return [];
    throw error;
  }
}

function installed(name) {
  const plugin = path.join(shims, name);
  return fs.existsSync(path.join(plugin, "tui.js")) ? plugin : undefined;
}

function configure() {
  const env = process.env;
  // Status runs in every app tab; the transcript sync only in tmux-backed ones.
  const status = env.HARNESS_AGENT_TAB_ID || env.HARNESS_AGENT_WORKTREE || env.HARNESS_REMOTE_SESSION
    ? installed("opencode-status") : undefined;
  const remote = env.HARNESS_REMOTE_SESSION ? installed("opencode-remote") : undefined;
  const ours = [status, remote].filter(Boolean);
  if (!ours.length) return;
  const inline = JSON.parse(env[cliConfig] || "{}");
  // Inline arrays replace global arrays, so preserve the effective plugin list before appending ours.
  const plugins = inline.plugins ?? configuredPlugins();
  if (!Array.isArray(plugins)) return;
  env[cliConfig] = JSON.stringify({ ...inline, plugins: [...plugins.filter((entry) => !ours.includes(entry)), ...ours] });
  if (remote) env.HARNESS_OPENCODE_REMOTE_DIR = path.resolve(shims, "../remote/opencode");
}

try {
  configure();
} catch {
  // An unreadable user configuration leaves the terminal on its normal launch path.
}

const [program, ...args] = process.argv.slice(2);
if (typeof process.execve === "function") process.execve(program, [program, ...args], process.env);
const result = spawnSync(program, args, { stdio: "inherit" });
process.exitCode = result.status ?? 1;
