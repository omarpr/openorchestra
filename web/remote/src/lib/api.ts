import { clearToken, getToken, setToken } from "./token";

export type AgentStatus = "running" | "waiting" | "idle" | "unknown" | "stopped";

/** Mirrors the server's `AgentInfo`. */
export interface Agent {
  id: string;
  title: string;
  agent_kind: string | null;
  project: string;
  worktree: string;
  branch: string | null;
  has_conversation: boolean;
  status: AgentStatus;
  status_since: string | null;
  tool: string | null;
}

export class UnauthorizedError extends Error {
  constructor() {
    super("unauthorized");
    this.name = "UnauthorizedError";
  }
}

/** Non-2xx response other than 401. `code` is the JSON `error` field when present. */
export class ApiError extends Error {
  constructor(
    public status: number,
    public code: string,
  ) {
    super(code || `HTTP ${status}`);
    this.name = "ApiError";
  }
}

export async function errorCode(res: Response): Promise<string> {
  try {
    const t = await res.text();
    try {
      const j = JSON.parse(t);
      if (j && typeof j.error === "string") return j.error;
    } catch {
      /* plain text body (e.g. 413) */
    }
    return t.slice(0, 120);
  } catch {
    return "";
  }
}

async function request(path: string, init: RequestInit = {}, auth = true): Promise<Response> {
  const headers = new Headers(init.headers);
  if (auth) {
    const t = getToken();
    if (t) headers.set("Authorization", `Bearer ${t}`);
  }
  const res = await fetch(path, { ...init, headers });
  if (res.status === 401 && auth) {
    clearToken();
    throw new UnauthorizedError();
  }
  if (!res.ok) throw new ApiError(res.status, await errorCode(res));
  return res;
}

/** Exchange a pairing code for a device token (also stored). A 401 here means a bad code. */
export async function pair(code: string, deviceName: string): Promise<string> {
  const res = await request(
    "/api/pair",
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ code, device_name: deviceName }),
    },
    false,
  );
  const { token } = (await res.json()) as { token: string };
  setToken(token);
  return token;
}

export interface Device {
  name: string;
  read_only: boolean;
  voice_input?: boolean;
}

export async function me(): Promise<Device> {
  return (await request("/api/me")).json();
}

export async function transcribeVoice(audio: Blob, signal: AbortSignal): Promise<string> {
  const response = await request("/api/voice", {
    method: "POST",
    headers: { "Content-Type": "audio/wav" },
    body: audio,
    signal,
  });
  const { text } = (await response.json()) as { text: string };
  return text;
}

export async function listAgents(): Promise<Agent[]> {
  return (await request("/api/agents")).json();
}

/**
 * Type `text` into the agent's terminal. Resolves with when the Mac did it, by
 * the Mac's clock (its `Date` header; the phone's clock may be off), in ms.
 */
export async function sendText(id: string, text: string): Promise<number> {
  const res = await request(`/api/agents/${encodeURIComponent(id)}/send`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ text }),
  });
  const date = Date.parse(res.headers.get("Date") ?? "");
  return Number.isNaN(date) ? Date.now() : date;
}

export const MAX_SCREENSHOT_BYTES = 8 * 1024 * 1024;

function readScreenshot(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      const url = String(reader.result);
      resolve(url.slice(url.indexOf(",") + 1));
    };
    reader.onerror = () => reject(new ApiError(0, "unreadable_image"));
    reader.readAsDataURL(file);
  });
}

export async function sendScreenshot(id: string, text: string, file: File): Promise<void> {
  const image = await readScreenshot(file);
  await request(`/api/agents/${encodeURIComponent(id)}/send-screenshot`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ text, image }),
  });
}

export interface ToolResult {
  text: string;
  is_error: boolean;
  input?: string;
  /** Output was cut at the server's cap; the UI shows the `…[truncated]` note. */
  truncated: boolean;
}

export async function fetchToolResult(id: string, toolId: string): Promise<ToolResult> {
  return (
    await request(`/api/agents/${encodeURIComponent(id)}/conversation/tool/${encodeURIComponent(toolId)}`)
  ).json();
}
