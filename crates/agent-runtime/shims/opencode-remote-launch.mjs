import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const cliConfig = "OPENCODE_CLI_CONFIG_CONTENT";
const shims = path.dirname(fileURLToPath(import.meta.url));

// OpenCode reads cli.json as JSONC (comments, trailing commas); JSON.parse
// would reject those and silently skip our plugins. Strip both outside
// strings, then parse.
function parseJsonc(text) {
  // Each pass copies strings untouched and rewrites only what is outside them.
  const outside = (source, rewrite) => {
    let out = "";
    for (let i = 0; i < source.length; i++) {
      if (source[i] === '"') {
        let end = i + 1;
        while (end < source.length && source[end] !== '"') end += source[end] === "\\" ? 2 : 1;
        out += source.slice(i, end + 1);
        i = end;
        continue;
      }
      const [kept, skip] = rewrite(source, i);
      out += kept;
      i += skip;
    }
    return out;
  };
  const uncommented = outside(text, (source, i) => {
    if (source.startsWith("//", i)) {
      const end = source.indexOf("\n", i);
      return ["\n", (end < 0 ? source.length : end) - i];
    }
    if (source.startsWith("/*", i)) {
      const end = source.indexOf("*/", i + 2);
      return [" ", (end < 0 ? source.length : end + 1) - i];
    }
    return [source[i], 0];
  });
  return JSON.parse(outside(uncommented, (source, i) =>
    source[i] === "," && /^\s*[}\]]/.test(source.slice(i + 1)) ? ["", 0] : [source[i], 0]));
}

function configuredPlugins() {
  const root = process.env.XDG_CONFIG_HOME || path.join(os.homedir(), ".config");
  const directory = path.join(root, "opencode");
  try {
    return (parseJsonc(fs.readFileSync(path.join(directory, "cli.json"), "utf8")).plugins ?? []).map((entry) => {
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
// Node 23.11+ has process.execve everywhere, but on Windows it throws
// ERR_FEATURE_UNAVAILABLE_ON_PLATFORM: run the program as a child there.
if (typeof process.execve === "function" && process.platform !== "win32") {
  process.execve(program, [program, ...args], process.env);
}
const result = spawnSync(program, args, { stdio: "inherit" });
// A program that cannot start at all (missing, or a script Node cannot run)
// would otherwise leave the tab at a prompt with no word of why.
if (result.error) console.error(`openOrchestra could not start ${program}: ${result.error.message}`);
process.exitCode = result.status ?? 1;
