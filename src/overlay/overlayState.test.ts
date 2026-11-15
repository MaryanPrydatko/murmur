// Standalone assert check (no JS unit-test runner in this repo). Run with:
//   bun src/overlay/overlayState.test.ts
import assert from "node:assert";
import {
  barHeights,
  formatTimer,
  visualState,
  type OverlayInputs,
} from "./overlayState";

const base: OverlayInputs = {
  session: null,
  captureReady: true,
  working: false,
  hasText: false,
  noticeLevel: null,
  meeting: false,
  controls: false,
  meetingWork: false,
};
const at = (i: Partial<OverlayInputs>) => visualState({ ...base, ...i });

assert.equal(at({}), "idle");
assert.equal(at({ session: "recording", captureReady: false }), "arming");
assert.equal(at({ session: "recording" }), "listening");
assert.equal(at({ session: "streaming" }), "listening");
assert.equal(at({ session: "streaming", hasText: true }), "live");
assert.equal(
  at({ session: "streaming", hasText: true, working: true }),
  "live-working",
);
assert.equal(at({ session: "streaming", working: true }), "transcribing");
assert.equal(at({ session: "processing" }), "processing");
assert.equal(at({ session: "notice", noticeLevel: "warning" }), "notice-warn");
assert.equal(
  at({ session: "live_notice", noticeLevel: "error" }),
  "notice-error",
);
assert.equal(at({ meeting: true }), "meeting");
assert.equal(at({ meeting: true, controls: true }), "meeting-controls");
assert.equal(at({ meeting: true, session: "recording" }), "meeting-dictating");
assert.equal(at({ meetingWork: true }), "meeting-transcribing");

// A lens: the middle bar reads the lowest bucket, the edges the highest.
const levels = Array.from({ length: 16 }, (_, i) => (i === 0 ? 1 : 0));
const bars = barHeights(levels, 13, 20);
assert.equal(bars.length, 13);
assert.equal(bars[6], 20);
assert.ok(bars.every((h, i) => i === 6 || h === 3));
assert.deepEqual(barHeights(Array(16).fill(0), 5, 17), [3, 3, 3, 3, 3]);

assert.equal(formatTimer(724), "12:04");
assert.equal(formatTimer(3725), "1:02:05");
console.log("overlayState: ok");
