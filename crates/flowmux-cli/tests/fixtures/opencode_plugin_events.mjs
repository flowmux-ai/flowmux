// SPDX-License-Identifier: GPL-3.0-or-later
import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { readFile } from "node:fs/promises";

const calls = [];
const completed = [];
let failNext = false;
globalThis.flowmuxTestSpawn = (_bin, args) => {
  if (failNext) {
    failNext = false;
    throw new Error("unavailable");
  }
  const child = new EventEmitter();
  child.unref = () => {};
  const event = args[args.indexOf("opencode") + 1];
  const payload = args.find((arg) => arg.startsWith("{"));
  calls.push({ event, payload: payload ? JSON.parse(payload) : null });
  setTimeout(() => {
    completed.push(event);
    child.emit("close", 0);
  }, event === "running" ? 40 : 1);
  return child;
};

const source = (await readFile(process.argv[2], "utf8")).replace(
  'import { spawn } from "node:child_process";',
  "const spawn = globalThis.flowmuxTestSpawn;",
);
const { server } = await import(`data:text/javascript;base64,${Buffer.from(source).toString("base64")}`);
const hooks = await server();
const emit = (type, properties) => hooks.event({ event: { type, properties } });

await emit("session.created", { info: { id: "root" } });
assert.deepEqual(calls.at(-1), { event: "session-start", payload: { session_id: "root" } });

await emit("session.error", { sessionID: "root", error: { name: "UnknownError" } });
assert.equal(calls.at(-1).payload.session_id, "root");
await emit("permission.asked", { sessionID: "root", id: "permission-1" });
assert.equal(calls.at(-1).payload.session_id, "root");
await emit("permission.replied", { sessionID: "root", requestID: "permission-1", reply: "once" });
assert.deepEqual(calls.at(-1), { event: "running", payload: { session_id: "root" } });

// A slow busy callback must finish before the newer idle callback is sent.
completed.length = 0;
await Promise.all([
  emit("session.status", { sessionID: "root", status: { type: "busy" } }),
  emit("session.status", { sessionID: "root", status: { type: "idle" } }),
]);
assert.deepEqual(completed, ["running", "stop"]);

await emit("session.deleted", { sessionID: "root", info: { id: "root" } });
assert.deepEqual(calls.at(-1), { event: "session-end", payload: { session_id: "root" } });

// A failed hook must not reject the plugin callback or poison its queue.
failNext = true;
await emit("session.status", { sessionID: "root-2", status: { type: "busy" } });
await emit("session.status", { sessionID: "root-2", status: { type: "idle" } });
assert.equal(completed.at(-1), "stop");
console.log("OpenCode lifecycle routing and ordering verified");
