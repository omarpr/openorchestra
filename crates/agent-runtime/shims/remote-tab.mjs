import { execFileSync } from "node:child_process";
import { createHash, randomBytes } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

export function writeStatusFiles(cwd, status) {
	const dir = path.join(process.env.HARNESS_AGENT_WORKTREE || cwd, ".harness");
	fs.mkdirSync(dir, { recursive: true });
	// Older tmux tabs already have HARNESS_REMOTE_SESSION from their launch.
	const id = process.env.HARNESS_AGENT_TAB_ID ??
		/^harness_[A-Za-z0-9_-]+_(\d+)$/.exec(process.env.HARNESS_REMOTE_SESSION ?? "")?.[1];
	const tabId = Number(id);
	const tagged = Number.isSafeInteger(tabId) && tabId > 0;
	const payload = JSON.stringify(tagged ? { ...status, tabId } : status, null, "\t") + "\n";
	const write = (file) => {
		const tmp = path.join(dir, `.status-${process.pid}-${randomBytes(4).toString("hex")}.tmp`);
		fs.writeFileSync(tmp, payload, "utf8");
		fs.renameSync(tmp, path.join(dir, file));
	};
	if (tagged) write(`status-tab-${tabId}.json`);
	write("status.json");
	return payload;
}

// The worktree-wide status file can be overwritten by a sibling agent before
// the host observes it. Keep one atomic record per actual harness tmux session.
export function recordTab(payload, agentSessionId) {
	const pane = process.env.TMUX_PANE;
	try {
		let session = process.env.HARNESS_REMOTE_SESSION;
		if (!session && /^%\d+$/.test(pane ?? "")) session = execFileSync("tmux", ["display-message", "-p", "-t", pane, "#{session_name}"], {
			encoding: "utf8", timeout: 500, stdio: ["ignore", "pipe", "ignore"],
		}).trim();
		if (!/^harness_[A-Za-z0-9_-]+_\d+$/.test(session)) return;
		const dir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../remote/tabs");
		fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
		// Match oo_remote::session_file_stem for long tmux session names.
		const stem = session.length <= 230 ? session : `long-${createHash("sha256").update(session).digest("hex")}`;
		const target = path.join(dir, stem + ".json");
		const next = JSON.parse(payload);
		next.agentSessionId = agentSessionId;
		next.statusSince = next.timestamp;
		if (agentSessionId) {
			try {
				const prev = JSON.parse(fs.readFileSync(target, "utf8"));
				if (prev.agent === next.agent && prev.agentSessionId === agentSessionId) {
					next.sessionFile ??= prev.sessionFile;
					if (prev.state === next.state) next.statusSince = prev.statusSince ?? prev.timestamp;
				}
			} catch { /* First event for this tab. */ }
		}
		const tmp = target + "." + process.pid;
		fs.writeFileSync(tmp, JSON.stringify(next), { mode: 0o600 });
		fs.renameSync(tmp, target);
	} catch { /* Status reporting must never interrupt the agent. */ }
}
