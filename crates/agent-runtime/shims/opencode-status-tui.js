// OpenCode 2 TUI plugin: report this tab's running / waiting / idle state.
// Installed as shims/opencode-status/tui.js; the logic is in status.mjs
// (opencode-status.mjs in the source tree) so node can test it.
import { Plugin } from "@opencode/plugin/tui";
import { startStatusReporter, statusWriter } from "./status.mjs";

export default Plugin.define({
  id: "openorchestra.status",
  setup(context) {
    // shims/remote-tab.mjs, shared with the other agents' hooks. Loaded
    // lazily so a missing helper only costs the status, never the TUI.
    const helper = new URL("../remote-tab.mjs", import.meta.url).href;
    const write = statusWriter(() => import(helper), process.cwd());
    return startStatusReporter(context, { write });
  },
});
