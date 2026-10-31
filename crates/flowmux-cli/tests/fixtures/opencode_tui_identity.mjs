// SPDX-License-Identifier: GPL-3.0-or-later
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const calls = [];
globalThis.flowmuxHook = async (event, payload) => calls.push({ event, payload });
let tick;
let cleared = false;
globalThis.setInterval = (fn) => { tick = fn; return 1; };
globalThis.clearInterval = () => { cleared = true; };
const source = (await readFile(process.argv[2], "utf8")).replace(
  'import { fireFlowmuxHook } from "./plugins/flowmux-session.js";',
  "const fireFlowmuxHook = globalThis.flowmuxHook;",
);
const { default: plugin } = await import(`data:text/javascript;base64,${Buffer.from(source).toString("base64")}`);
let dispose;
let event;
let unsubscribed = false;
let permissions = [];
let questions = [];
let query = async () => ({ data: {} });
const api = {
  route: { current: { name: "session", params: { sessionID: "root" } } },
  state: { ready: false, session: {
    get: (id) => id === "missing" ? undefined : { id },
    permission: () => permissions, question: () => questions,
  } },
  client: { session: { status: () => query() } },
  event: { on: (_type, fn) => { event = fn; return () => { unsubscribed = true; }; } },
  lifecycle: { onDispose: (fn) => { dispose = fn; } },
};
const route = (id) => { api.route.current = { name: "session", params: { sessionID: id } }; };
delete process.env.FLOWMUX_SURFACE_ID;
await plugin.tui(api);
assert.equal(tick, undefined, "no timer outside Flowmux");
process.env.FLOWMUX_SURFACE_ID = "test-surface";
await plugin.tui(api);
assert.equal(calls.length, 0, "loading does not imply idle");
api.state.ready = true;
query = async () => ({ error: "offline" });
await tick();
assert.equal(calls.length, 0);
query = async () => ({ data: {} });
await tick();
assert.deepEqual(calls.pop(), { event: "session-start", payload: {
  session_id: "root", source: "tui-route", session_status: "idle",
} });
await tick();
assert.equal(calls.length, 0, "same route does not repeatedly reset activity");
route("child");
query = async () => ({ data: { child: { type: "busy" } } });
await tick();
assert.equal(calls.pop().payload.session_status, "busy");

route("waiting");
permissions = [{ id: "approval" }];
await tick();
assert.equal(calls.pop().payload.session_status, "needs_input");
permissions = [];
route("question");
questions = [{ id: "question" }];
await tick();
assert.equal(calls.pop().payload.session_status, "needs_input");
questions = [];

// A route switch or newer lifecycle event invalidates an in-flight status read.
let resolve;
query = () => new Promise((done) => { resolve = done; });
route("old");
const pending = tick();
route("new");
resolve({ data: {} });
await pending;
assert.equal(calls.length, 0);
const newer = tick();
event({ properties: { sessionID: "new", status: { type: "busy" } } });
resolve({ data: {} });
await newer;
assert.equal(calls.length, 0);
query = async () => ({ data: { new: { type: "retry" } } });
await tick();
assert.equal(calls.pop().payload.session_status, "retry");

api.route.current = { name: "home" };
await tick();
assert.deepEqual(calls.pop(), { event: "session-end", payload: { session_id: "new" } });
await tick();
route("missing");
await tick();
assert.equal(calls.length, 0, "unknown native session is not an identity");
route("disposed");
query = () => new Promise((done) => { resolve = done; });
const final = tick();
dispose();
resolve({ data: {} });
await final;
await tick();
assert.equal(calls.length, 0);
assert.ok(cleared && unsubscribed);
console.log("OpenCode native route identity, status, races and disposal verified");
