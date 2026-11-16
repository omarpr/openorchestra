import { clearToken, getToken, setToken } from "./token";

export type AgentStatus = "running" | "waiting" | "idle" | "unknown" | "stopped";

/** Mirrors the server's `AgentInfo`. */
export interface Agent {
  id: string;
  title: string;
  agent_kind: string | null;
  project: string;
  worktree: string;
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

async function errorCode(res: Response): Promise<string> {
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

export async function me(): Promise<{ name: string; read_only: boolean }> {
  return (await request("/api/me")).json();
}

export async function listAgents(): Promise<Agent[]> {
  return (await request("/api/agents")).json();
}

export async function sendText(id: string, text: string): Promise<void> {
  await request(`/api/agents/${encodeURIComponent(id)}/send`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ text }),
  });
}
