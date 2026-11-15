// Standalone assert check (no JS unit-test runner in this repo). Run with:
//   bun src/components/settings/meetings/meetingModel.test.ts
import assert from "node:assert/strict";
import { playbackStart } from "./meetingModel";

const offsets = [2.059, 2.105];
assert.equal(playbackStart(0, 571, offsets), 2.059, "play skips the lead-in");
assert.equal(playbackStart(571, 571, offsets), 2.059, "replay too");
assert.equal(playbackStart(1, 571, offsets), 2.059, "and a seek into it");
assert.equal(playbackStart(60, 571, offsets), 60, "later times stay put");
assert.equal(playbackStart(0, 10, [0]), 0);

console.log("meetingModel: all assertions passed");
