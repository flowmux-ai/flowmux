// SPDX-License-Identifier: GPL-3.0-or-later
// flowmux-opencode-tui-plugin v1
// Installed by `flowmux hooks setup`; observes the selected native session.
import { fireFlowmuxHook } from "./plugins/flowmux-session.js";

export default {
  id: "flowmux-session-tui",
  tui: async (api) => {
    if (!process.env.FLOWMUX_SURFACE_ID) return;
    let selected;
    let pending = false;
    let disposed = false;
    let revision = 0;
    const unsubscribe = api.event.on("session.status", () => { revision += 1; });
    const current = () => {
      const route = api.route.current;
      return route.name === "session" ? route.params?.sessionID : undefined;
    };
    const poll = async () => {
      if (disposed || pending || !api.state.ready) return;
      const id = current();
      if (!id) {
        if (selected && api.route.current.name === "home") {
          const previous = selected;
          selected = undefined;
          await fireFlowmuxHook("session-end", { session_id: previous });
        }
        return;
      }
      if (id === selected || !api.state.session.get(id)) return;
      pending = true;
      const started = revision;
      try {
        // The native status map omits idle sessions. Wait for a successful
        // response; an empty, not-yet-hydrated TUI cache does not prove idle.
        const result = await api.client.session.status();
        if (disposed || current() !== id || revision !== started || result.error ||
            !result.data || typeof result.data !== "object" || Array.isArray(result.data)) return;
        const waiting = api.state.session.permission(id).length || api.state.session.question(id).length;
        const status = waiting ? "needs_input" : (result.data[id]?.type ?? "idle");
        await fireFlowmuxHook("session-start", {
          session_id: id, source: "tui-route", session_status: status,
        });
        if (!disposed && current() === id) selected = id;
      } catch (_) {
        // Keep the agent usable and retry observation on the next tick.
      } finally {
        pending = false;
      }
    };
    const timer = setInterval(poll, 500);
    timer.unref?.();
    api.lifecycle.onDispose(() => {
      disposed = true;
      clearInterval(timer);
      unsubscribe();
    });
    await poll();
  },
};
