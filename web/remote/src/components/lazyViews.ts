import { lazy } from "react";

// The heavy views load on first use: xterm and the Markdown stack aren't needed for the agent
// list or pairing, so a phone opening the app downloads far less up front. Tests replace these
// with the real components (test-setup.ts), keeping their renders synchronous.

export const TerminalView = lazy(() => import("./TerminalView").then((m) => ({ default: m.TerminalView })));

export const ConversationView = lazy(() => import("./ConversationView").then((m) => ({ default: m.ConversationView })));
