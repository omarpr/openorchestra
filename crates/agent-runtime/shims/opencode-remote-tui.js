import fs from "node:fs";
import path from "node:path";
import { Plugin } from "@opencode/plugin/tui";

const MAX_ROWS = 200;
const MAX_BYTES = 2 * 1024 * 1024;
const TEXT_LIMIT = 16 * 1024;

function truncate(text, max) {
  const safe = typeof text.toWellFormed === "function" ? text.toWellFormed() : text;
  if (safe.length <= max) return safe;
  let end = max;
  const code = safe.charCodeAt(end - 1);
  if (code >= 0xd800 && code <= 0xdbff) end -= 1;
  return safe.slice(0, end);
}

function firstLine(text) {
  const line = text?.trim().split("\n", 1)[0] ?? "";
  return line.length > 80 ? `${truncate(line, 80)}…` : line;
}

function assistantEntry(id, at, part) {
  if (part.type === "text" && part.text?.trim()) {
    return { id, at, kind: "assistant", text: truncate(part.text, TEXT_LIMIT) };
  }
  if (part.type !== "tool" || typeof part.name !== "string") return null;

  const state = part.state;
  const command = state?.input?.command;
  const detail = typeof command === "string" ? command : state?.input?.filePath;
  const status = state?.status === "completed" ? "ok" : state?.status === "error" ? "error" : "pending";
  const result = (state?.content ?? []).filter((item) => item.type === "text").map((item) => item.text).join("\n");
  const tool = { id, name: part.name, status, summary: firstLine(typeof detail === "string" ? detail : "") };
  const entry = { id, at, kind: "tool", tool };
  if (status !== "pending") {
    const output = result || (status === "error" ? state.error?.message ?? "Tool failed" : "");
    entry.result = {
      text: truncate(output, TEXT_LIMIT),
      is_error: status === "error",
      truncated: output.length > TEXT_LIMIT,
      ...(typeof command === "string" && command ? { input: truncate(command, TEXT_LIMIT) } : {}),
    };
  }
  return entry;
}

function entries(sessionID, messages) {
  const rows = [];
  for (const message of messages.slice(-MAX_ROWS)) {
    const id = `${sessionID}:${message.id}`;
    const millis = message.time?.created;
    const at = Number.isFinite(millis) ? new Date(millis).toISOString() : undefined;
    if (message.type === "user" && message.text?.trim()) {
      rows.push({ id, at, kind: "user", text: truncate(message.text, TEXT_LIMIT) });
    }
    if (message.type === "assistant") {
      for (const [index, part] of (message.content ?? []).entries()) {
        const row = assistantEntry(`${id}:${index}`, at, part);
        if (row) rows.push(row);
      }
    }
  }
  const lines = [];
  let bytes = 0;
  for (const row of rows.slice(-MAX_ROWS).reverse()) {
    const line = `${JSON.stringify(row)}\n`;
    bytes += Buffer.byteLength(line);
    if (bytes > MAX_BYTES) break;
    lines.push(line);
  }
  return lines.reverse().join("");
}

export default Plugin.define({
  id: "openorchestra.remote-conversation",
  setup(context) {
    const directory = process.env.HARNESS_OPENCODE_REMOTE_DIR;
    const tab = process.env.HARNESS_REMOTE_SESSION ?? "";
    if (!directory || !/^harness_[A-Za-z0-9_-]+_\d+$/.test(tab)) return;

    const file = path.join(directory, `${tab}.jsonl`);
    const flag = `${file}.on`;
    const tmp = `${file}.${process.pid}.tmp`;
    let previous;
    let sessionID = null;
    let dirty = true;

    function update() {
      try {
        const route = context.ui.router.current();
        const next = fs.existsSync(flag) && route.type === "session" ? route.sessionID : undefined;
        if (next !== sessionID) {
          sessionID = next;
          previous = undefined;
          dirty = true;
          fs.rmSync(file, { force: true });
        }
        if (!next || (!dirty && fs.existsSync(file))) return;
        const messages = context.data.session.message.list(next);
        if (!messages) return;
        const contents = entries(next, messages);
        if (contents !== previous || !fs.existsSync(file)) {
          fs.writeFileSync(tmp, contents, { mode: 0o600 });
          if (fs.existsSync(flag)) fs.renameSync(tmp, file);
          else fs.rmSync(tmp, { force: true });
          previous = contents;
        }
        dirty = false;
      } catch {
        // A transcript sync must never interrupt the terminal.
      }
    }

    const stop = context.data.listen(() => { dirty = true; });
    const timer = setInterval(update, 750);
    update();
    return () => {
      stop();
      clearInterval(timer);
      try {
        fs.rmSync(file, { force: true });
        fs.rmSync(tmp, { force: true });
      } catch { /* The app may have already removed the files. */ }
    };
  },
});
