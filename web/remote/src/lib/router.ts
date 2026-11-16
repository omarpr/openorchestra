import { useSyncExternalStore } from "react";

export type Route = { name: "list" } | { name: "agent"; id: string };

export function parseHash(hash: string): Route {
  const m = /^#\/agent\/([^/?]+)/.exec(hash);
  if (m) {
    try {
      return { name: "agent", id: decodeURIComponent(m[1]) };
    } catch {
      /* fall through */
    }
  }
  return { name: "list" };
}

export function agentHref(id: string): string {
  return `#/agent/${encodeURIComponent(id)}`;
}

export function navigate(hash: string): void {
  window.location.hash = hash;
}

function subscribe(cb: () => void) {
  window.addEventListener("hashchange", cb);
  return () => window.removeEventListener("hashchange", cb);
}

const snapshot = () => window.location.hash;

/** Current hash route; re-renders on `hashchange`. */
export function useRoute(): Route {
  const hash = useSyncExternalStore(subscribe, snapshot);
  return parseHash(hash);
}
