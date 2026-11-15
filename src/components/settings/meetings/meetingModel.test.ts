// Standalone assert check (no JS unit-test runner in this repo). Run with:
//   bun src/components/settings/meetings/meetingModel.test.ts
import assert from "node:assert/strict";
import { playbackStart } from "./meetingModel";

const offsets = [2.059, 2.105];
assert.equal(playbackStart(0, offsets), 2.059, "play skips the lead-in");
assert.equal(playbackStart(1, offsets), 2.059, "and so does a seek into it");
assert.equal(playbackStart(60, offsets), 60, "later times stay put");
assert.equal(playbackStart(571, offsets), 571, "the end stays the end");
assert.equal(playbackStart(0, [0]), 0);

console.log("meetingModel: all assertions passed");
