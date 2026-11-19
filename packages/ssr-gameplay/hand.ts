// OCR calibration for one Japanese battle layout. No input is issued here.
export const battle: MadoRecognitionBasis = {
  frame_width: 2102, frame_height: 1640,
  content: { x: 0, y: 260, width: 2102, height: 1184 },
};
const cards = { x: 307, y: 1111, width: 1453, height: 183 };
const count = { x: 996, y: 1391, width: 151, height: 44 };
const ap = { x: 1016, y: 1323, width: 69, height: 68 };

function zone(id: string, box: MadoRegion) {
  const { x, y, width, height } = battle.content;
  return { id, region: {
    u0: (box.x - x) / width, v0: (box.y - y) / height,
    u1: (box.x + box.width - x) / width, v1: (box.y + box.height - y) / height,
  } };
}
export const zones = [zone("cards", cards), zone("hand-count", count), zone("ap", ap)];

type Zone = MadoOcrZoneScanResult["zones"][number];
type Line = Zone["regions"][number];
const minConfidence = 0.9;
const typeLabels = ["基本攻撃", "攻撃", "基本スキル"];
export const NO_HAND_HUD = "Sortie: hand HUD not observed.";
const ambiguous = "Sortie: ambiguous hand candidate; no input.";

function centeredIn(line: Line, box: MadoRegion): boolean {
  const x = line.bounds.x + line.bounds.width / 2;
  const y = line.bounds.y + line.bounds.height / 2;
  return x >= box.x && x < box.x + box.width && y >= box.y && y < box.y + box.height;
}

function hudValue(scan: Zone, box: MadoRegion): string {
  const line = scan.regions[0];
  if (scan.outcome !== "recognized" || scan.regions.length !== 1 || !line ||
      line.confidence < minConfidence || !centeredIn(line, box)) return "";
  return line.text;
}

// A fanned card's title and type boxes can overlap vertically.
function labeledBy(title: MadoRegion, caption: MadoRegion): boolean {
  const overlap = Math.min(title.x + title.width, caption.x + caption.width) - Math.max(title.x, caption.x);
  const dy = (caption.y + caption.height / 2) - (title.y + title.height / 2);
  return overlap >= Math.min(title.width, caption.width) / 2 && dy > 0 && dy <= title.height;
}

function costBesideTitle(cost: MadoRegion, title: MadoRegion): boolean {
  const gap = title.x - (cost.x + cost.width);
  const offset = Math.abs((cost.y + cost.height / 2) - (title.y + title.height / 2));
  return gap >= 0 && gap <= cost.height && offset <= Math.min(cost.height, title.height) / 2;
}

function readHand(scan: readonly Zone[]) {
  const [hand, size, points] = scan;
  if (!hand || hand.id !== "cards" || !size || size.id !== "hand-count" || !points || points.id !== "ap") {
    return "Sortie: hand scan incomplete; no input.";
  }
  if (size.outcome === "no_match" && points.outcome === "no_match") return NO_HAND_HUD;
  const countText = hudValue(size, count);
  const apText = hudValue(points, ap);
  if (!/^(?:0?[0-9]|10)\/10$/.test(countText) || !/^[0-9]$/.test(apText)) {
    return "Sortie: hand HUD unreadable; no input.";
  }
  const handSize = Number.parseInt(countText, 10);
  if (handSize === 0) return "Sortie: zero hand count; no input.";
  if (hand.outcome === "no_match") return "Sortie: hand labels unreadable; no input.";
  const lines = hand.regions.filter(line => centeredIn(line, cards));
  const types = lines.filter(line => typeLabels.includes(line.text));
  if (types.length > handSize) return ambiguous;
  if (!types.length) return "Sortie: hand labels unreadable; no input.";
  const titles = lines.filter(line => line.text !== "" && !/^[0-9]+$/.test(line.text) && !typeLabels.includes(line.text));
  const costs = lines.filter(line => /^[0-9]$/.test(line.text));
  return { handSize, ap: Number(apText), types, titles, costs };
}

export function chooseHand(scan: readonly Zone[], priority: readonly string[], excluded: readonly string[]) {
  const hand = readHand(scan);
  if (typeof hand === "string") return hand;
  const { titles, types, costs } = hand;
  let overBudget = false;
  for (let i = 0; i < priority.length; i++) {
    const name = priority[i];
    if (!name || excluded.includes(name)) continue;
    const matches = titles.filter(line => line.text === name);
    const title = matches[0];
    if (!title) continue;
    if (matches.length !== 1 || title.confidence < minConfidence) return ambiguous;
    const captions = types.filter(type => labeledBy(title.bounds, type.bounds));
    const caption = captions[0];
    if (!caption || captions.length !== 1 || caption.confidence < minConfidence ||
        titles.some(other => other !== title && labeledBy(other.bounds, caption.bounds))) return ambiguous;
    const adjacent = costs.filter(line => costBesideTitle(line.bounds, title.bounds));
    const cost = adjacent[0];
    if (cost) {
      if (adjacent.length !== 1 || cost.confidence < minConfidence ||
          titles.some(other => other !== title && costBesideTitle(cost.bounds, other.bounds))) return ambiguous;
      if (Number(cost.text) > hand.ap) {
        overBudget = true;
        continue;
      }
    }
    return { title, caption, cost: cost?.text, handSize: hand.handSize, ap: hand.ap,
      message: "Sortie: visible card priority " + (i + 1) + "; verify playability and target manually." };
  }
  if (overBudget) return "Sortie: matched card costs exceed observed AP; no input.";
  return "Sortie: no matching non-excluded card title; no input.";
}

export function rankHand(scan: readonly Zone[], priority: readonly string[], excluded: readonly string[]): string {
  const choice = chooseHand(scan, priority, excluded);
  return typeof choice === "string" ? choice : choice.message;
}

export function consumed(scan: readonly Zone[], title: string, beforeSize: number, beforeAp: number): boolean {
  const hand = readHand(scan);
  return typeof hand !== "string" && hand.handSize === beforeSize - 1 && hand.ap === beforeAp - 1 &&
    !hand.titles.some(line => line.text === title) && hand.types.some(type => type.confidence >= minConfidence) &&
    hand.titles.some(line => line.confidence >= minConfidence && hand.types.some(type => labeledBy(line.bounds, type.bounds)));
}
