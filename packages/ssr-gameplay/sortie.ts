import { battle, chooseHand, consumed, rankHand, zones } from "./hand.js";
import { newer } from "./screen.js";
export { NO_HAND_HUD } from "./hand.js";

const magneticField = "マグネティックフィールド";
// The first crop includes the lit cost, card face and the full release path.
// One placement per crop: the SDK's first match is not a uniqueness test.
const skillLane = { x: 480, y: 952, width: 280, height: 336 };
const turnControl = { x: 1780, y: 1192, width: 184, height: 184 };

function matchedCrop(ctx: MadoWorkflowContext, observation: MadoObservation, asset: string, roi: MadoRegion): MadoRegion | null {
  host.call("asset", { id: asset });
  const match = ctx.call("recognize", { observation, kind: "template", asset, roi });
  if (!match) return null;
  try {
    const box = match.region;
    return match.score >= 0.995 && box.x === roi.x && box.y === roi.y &&
      box.width === roi.width && box.height === roi.height ? box : null;
  } finally {
    ctx.call("release", { id: match.id });
  }
}

function insideSlot(box: MadoRegion): boolean {
  return box.x >= skillLane.x && box.y >= skillLane.y &&
    box.x + box.width <= skillLane.x + skillLane.width && box.y + box.height <= skillLane.y + skillLane.height;
}

export function sortie(ctx: MadoWorkflowContext, priority: readonly string[], excluded: readonly string[], playMagneticField = false): string {
  const before = ctx.call("observe", {});
  try {
    if (before.width !== battle.frame_width || before.height !== battle.frame_height) {
      return "Sortie: unsupported hand layout; no input.";
    }
    const scan = ctx.call("scan_ocr_zones", { observation: before, basis: battle, zones });
    if (!playMagneticField) return rankHand(scan.zones, priority, excluded);
    const choice = chooseHand(scan.zones, priority, excluded);
    if (typeof choice === "string") return choice;
    if (choice.title.text !== magneticField) return choice.message;
    // A missing higher-ranked title need not be absent from a partly read hand.
    if (priority.find(name => !excluded.includes(name)) !== magneticField ||
        choice.handSize !== 4 || choice.ap !== 2 || choice.caption.text !== "基本スキル" ||
        (choice.cost !== undefined && choice.cost !== "1") ||
        !insideSlot(choice.title.bounds) || !insideSlot(choice.caption.bounds)) {
      return "Sortie: skill conditions differ from the reviewed four-card hand; no input.";
    }
    const lane = matchedCrop(ctx, before, "magnetic-field-ready", skillLane);
    if (!lane || !matchedCrop(ctx, before, "player-turn-ready", turnControl)) {
      return "Sortie: enabled skill, release path or turn control not confirmed; no input.";
    }
    const x = lane.x + lane.width / 2;
    const inset = lane.height / 12;
    const sequence = ctx.call("submit", { observation: before, actions: [
      { kind: "drag", button: "left", path: [
        { x, y: lane.y + lane.height - inset, hold_ms: 50 },
        { x, y: lane.y + inset, hold_ms: 500 },
      ] },
    ] });
    let receipt: MadoReceipt;
    try {
      receipt = ctx.call("settle", { id: sequence.id });
    } finally {
      ctx.call("release", { id: sequence.id });
    }
    if (receipt.status !== "Submitted" || receipt.submitted !== 1 || receipt.total !== 1 ||
        receipt.submitted_events !== 6 || receipt.total_events !== 6 || receipt.partial_native_effect ||
        receipt.used_fallback || receipt.cleanup_required.length || receipt.cleanup?.may_leave_state_held) {
      return "Sortie: skill drag not completely submitted; inspect receipt and cleanup. No retry.";
    }
    for (let second = 0; second < 5; second++) ctx.call("wait", { duration_ms: 1000 });
    const after = ctx.call("observe", {});
    try {
      if (!newer(before, after)) return "Sortie: no newer compatible hand observation; no retry.";
      const next = ctx.call("scan_ocr_zones", { observation: after, basis: battle, zones });
      if (!consumed(next.zones, magneticField, choice.handSize, choice.ap)) {
        return "Sortie: skill consumption not confirmed; no retry.";
      }
      return "Sortie: skill drag submitted; one fewer card and one less AP observed. Shield not verified.";
    } finally {
      ctx.call("release", { id: after.id });
    }
  } finally {
    ctx.call("release", { id: before.id });
  }
}
