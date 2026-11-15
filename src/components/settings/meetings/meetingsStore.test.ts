// Standalone assert check (no JS unit-test runner in this repo). Run with:
//   bun src/components/settings/meetings/meetingsStore.test.ts
import assert from "node:assert/strict";

const calls: string[] = [];
const asked: unknown[] = [];
let failToggle = false;
(globalThis as Record<string, unknown>).window = globalThis;
(globalThis as Record<string, unknown>).__TAURI_INTERNALS__ = {
  invoke: async (cmd: string, args?: { recording?: boolean }) => {
    calls.push(cmd);
    if (cmd === "set_meeting_recording") {
      asked.push(args?.recording);
      if (failToggle) throw "ipc down";
    }
    if (cmd === "list_meetings" || cmd === "list_meeting_speakers") return [];
    if (cmd === "is_meeting_recording") return false;
    return null;
  },
};

const { useMeetingsStore } = await import("./meetingsStore");
const store = useMeetingsStore.getState;
const toggles = () => calls.filter((c) => c === "set_meeting_recording").length;

await Promise.all([store().toggle(), store().toggle()]);
assert.equal(toggles(), 1, "a second click while pending is ignored");
assert.equal(store().pending, true);
assert.deepEqual(asked, [true], "with nothing recording, it asks to start");

await store().changed();
assert.equal(store().pending, false, "meetings-changed ends the wait");

await store().toggle();
assert.equal(toggles(), 2, "the next click goes through");
await store().changed();

useMeetingsStore.setState({ recordingId: "2026-11-15_14-03-22" });
await store().toggle();
assert.equal(
  asked[asked.length - 1],
  false,
  "while one records, it asks to stop",
);
await store().changed();

failToggle = true;
await store().toggle();
assert.equal(store().pending, false, "a failed toggle doesn't stay pending");
assert.equal(store().error, "ipc down");

console.log("meetingsStore: all assertions passed");
process.exit(0);
