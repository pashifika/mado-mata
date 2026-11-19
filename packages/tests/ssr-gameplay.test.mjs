import assert from "node:assert/strict";
import test from "node:test";
import { consumed, rankHand } from "../ssr-gameplay/hand.ts";
import { hand, line } from "./hand-fixture.mjs";

test("hand recommendation respects profile order, exclusions and visible cost", () => {
  const order = ["Beta", "Alpha"];
  assert.match(rankHand(hand(), order, []), /visible card priority 1;/);
  assert.match(rankHand(hand(), order, ["Beta"]), /visible card priority 2;/);
  assert.match(rankHand(hand(line("3", 722, 1140, 28, 36)), order, []), /visible card priority 2;/);
});

test("a duplicate preferred title does not silently fall through to another card", () => {
  const duplicates = hand(line("Beta", 700, 1140, 110, 36));
  assert.match(rankHand(duplicates, ["Beta", "Alpha"], []), /ambiguous hand candidate/);
});

test("a reduced hand without a readable AP change does not confirm consumption", () => {
  const scan = hand();
  assert.equal(consumed(scan, "Played", 3, 3), true);
  assert.equal(consumed(scan, "Alpha", 3, 3), false);
  scan[2].outcome = "no_match";
  scan[2].regions = [];
  assert.equal(consumed(scan, "Played", 3, 3), false);
});
