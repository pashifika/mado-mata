import { newer, submittedClick } from "./screen.js";

// Non-battle progression for the Japanese SSR client, calibrated on retained
// 2102×1640 frames. Pages without an SSR observation are not recognized.
const basis: MadoRecognitionBasis = {
  frame_width: 2102, frame_height: 1640,
  content: { x: 0, y: 260, width: 2102, height: 1182 },
};

function zone(id: string, box: MadoRegion) {
  const { x, y, width, height } = basis.content;
  return { id, region: {
    u0: (box.x - x) / width, v0: (box.y - y) / height,
    u1: (box.x + box.width - x) / width, v1: (box.y + box.height - y) / height,
  } };
}

type Zone = MadoOcrZoneScanResult["zones"][number];
type Line = Zone["regions"][number];
const minConfidence = 0.9;

// progressed: input was submitted and a strictly newer frame showed the expected
// change. handoff: no further automatic input on this scene; reason is readable.
export type Outcome =
  | { readonly kind: "progressed"; readonly page: string; readonly action: string }
  | { readonly kind: "handoff"; readonly page: string | null; readonly reason: string };

const unsupportedGeometry: Outcome = {
  kind: "handoff", page: null,
  reason: "Capture is not the calibrated 2102×1640 SSR layout; no input.",
};

function plain(text: string): string {
  return text.normalize("NFKC").replace(/\s+/gu, "");
}

function within(line: Line, box: MadoRegion): boolean {
  const x = line.bounds.x + line.bounds.width / 2;
  const y = line.bounds.y + line.bounds.height / 2;
  return x >= box.x && x < box.x + box.width && y >= box.y && y < box.y + box.height;
}

function confidentLines(scan: Zone, box: MadoRegion): Line[] {
  return scan.outcome === "recognized"
    ? scan.regions.filter(line => line.confidence >= minConfidence && within(line, box)) : [];
}

// Observed OCR may prefix an icon as one or two glyphs.
function labelled(text: string, label: string): boolean {
  return text === label || (text.endsWith(label) && text.length <= label.length + 2);
}

// ---- Card removal (排除): recommendation only ----
// The reference enrolls a target member before removing; that identity is not
// established here, so the Script never selects a card or presses 排除.
const removalHeader = { x: 100, y: 270, width: 1100, height: 100 };
const removalCards = { x: 560, y: 380, width: 1420, height: 920 };
const removalActions = { x: 1300, y: 1310, width: 802, height: 110 };
const removalZones = [
  zone("removal-header", removalHeader), zone("removal-cards", removalCards), zone("removal-actions", removalActions),
];
const removalPrompt = /^排除するカードを([0-9]+)枚選択してください。?$/;
const typeLabel = /^.{0,2}?(基本)?(?:攻[撃擊]|スキル)$/u;

export interface RemovalPolicy {
  readonly priority: readonly string[];
  readonly preferBasic: boolean;
}
export interface RemovalCard {
  readonly name: string;
  readonly basic: boolean;
  // Unique title/type pairing with confident readings.
  readonly certain: boolean;
  // Capture-pixel center, used only for ordering.
  readonly x: number;
  readonly y: number;
}
export interface RemovalPage {
  readonly count: number;
  readonly cards: readonly RemovalCard[];
}
export type RemovalChoice =
  | { readonly kind: "recommend"; readonly rule: "priority" | "basic"; readonly card: RemovalCard }
  | { readonly kind: "refuse"; readonly reason: "count" | "unreadable" | "ambiguous" | "none" };

// Type label directly below the title and overlapping it horizontally.
function captions(title: MadoRegion, type: MadoRegion): boolean {
  const overlap = Math.min(title.x + title.width, type.x + type.width) - Math.max(title.x, type.x);
  const gap = type.y - (title.y + title.height);
  return overlap >= Math.min(title.width, type.width) / 2 &&
    type.y + type.height / 2 > title.y + title.height / 2 &&
    gap >= -type.height && gap <= type.height;
}

export function readRemovalPage(scan: readonly Zone[]): RemovalPage | null {
  const [top, grid, bottom] = scan;
  if (top?.id !== "removal-header" || grid?.id !== "removal-cards" || bottom?.id !== "removal-actions") return null;
  const heading = confidentLines(top, removalHeader)
    .map(line => removalPrompt.exec(plain(line.text))).find(match => match !== null);
  const buttons = confidentLines(bottom, removalActions).map(line => plain(line.text));
  if (!heading || !buttons.some(text => labelled(text, "スキップ")) || !buttons.some(text => labelled(text, "排除"))) {
    return null;
  }
  const lines = grid.outcome === "recognized" ? grid.regions.filter(line => within(line, removalCards)) : [];
  const types = lines.flatMap(line => {
    const match = typeLabel.exec(plain(line.text));
    return match ? [{ line, basic: match[1] !== undefined }] : [];
  });
  const titles = lines.flatMap(line => {
    const name = plain(line.text).replace(/^[0-9:]+/u, "");
    return name && !typeLabel.test(plain(line.text)) ? [{ line, name }] : [];
  });
  const found: RemovalCard[] = [];
  for (const title of titles) {
    const below = types.filter(type => captions(title.line.bounds, type.line.bounds));
    const type = below[0];
    if (!type) continue;
    const unique = below.length === 1 &&
      titles.every(other => other === title || !captions(other.line.bounds, type.line.bounds));
    const left = Math.min(title.line.bounds.x, type.line.bounds.x);
    const right = Math.max(title.line.bounds.x + title.line.bounds.width, type.line.bounds.x + type.line.bounds.width);
    found.push({
      name: title.name,
      basic: unique && type.basic,
      certain: unique && title.line.confidence >= minConfidence && type.line.confidence >= minConfidence,
      x: (left + right) / 2,
      y: (title.line.bounds.y + type.line.bounds.y + type.line.bounds.height) / 2,
    });
  }
  return { count: Number(heading[1]), cards: found };
}

// Profile names must equal the displayed name. A visible card whose reading
// contains, or is contained in, the first present name without an exact
// confident reading blocks every lower choice.
export function chooseRemoval(page: RemovalPage, policy: RemovalPolicy): RemovalChoice {
  if (page.count !== 1) return { kind: "refuse", reason: "count" };
  if (!page.cards.length) return { kind: "refuse", reason: "unreadable" };
  // Bottom row first, then right to left, as in the reference. Card rows are
  // about 500 capture pixels apart; OCR centers within a row differ far less.
  const order = [...page.cards].sort((a, b) => Math.abs(b.y - a.y) > 100 ? b.y - a.y : b.x - a.x);
  for (const wanted of policy.priority) {
    const name = plain(wanted);
    if (!name) continue;
    const exact = order.filter(card => card.name === name);
    const card = exact.find(candidate => candidate.certain);
    if (card) return { kind: "recommend", rule: "priority", card };
    if (exact.length || order.some(other => other.name.includes(name) || name.includes(other.name))) {
      return { kind: "refuse", reason: "ambiguous" };
    }
  }
  if (!policy.preferBasic) return { kind: "refuse", reason: "none" };
  if (policy.priority.some(name => plain(name)) && order.some(card => !card.certain)) {
    return { kind: "refuse", reason: "ambiguous" };
  }
  const card = order.find(candidate => candidate.certain && candidate.basic);
  return card ? { kind: "recommend", rule: "basic", card } : { kind: "refuse", reason: "none" };
}

const removalRefusals = {
  count: "asks for other than one card",
  unreadable: "no card title/type pair was read",
  ambiguous: "a configured name is not uniquely and confidently readable",
  none: "no visible card matches the policy",
} as const;

// Observation only: returns null when the page is not the removal page.
export function recommendRemoval(ctx: MadoWorkflowContext, policy: RemovalPolicy): Outcome | null {
  const observation = ctx.call("observe", {});
  try {
    if (!calibrated(observation)) return unsupportedGeometry;
    const page = readRemovalPage(ctx.call("scan_ocr_zones", { observation, basis, zones: removalZones }).zones);
    if (!page) return null;
    const choice = chooseRemoval(page, policy);
    return {
      kind: "handoff", page: "card-removal",
      reason: choice.kind === "recommend"
        ? "Card removal: recommend " + choice.card.name + " (" + choice.rule + "); select and press 排除 manually."
        : "Card removal: " + removalRefusals[choice.reason] + "; no input.",
    };
  } finally {
    ctx.call("release", { id: observation.id });
  }
}

// ---- Story: fork, act catalog, chapter map and team setup ----
// Opening the Story tile is navigation only; the combat tile is never chosen.
// Act, episode, team and stage choices stay with the operator.
const storyHeader = { x: 120, y: 270, width: 420, height: 100 };
const forkStory = { x: 420, y: 1110, width: 620, height: 90 };
const forkCombat = { x: 1240, y: 1110, width: 620, height: 90 };
const actBand = { x: 0, y: 990, width: 2102, height: 110 };
const chapterTitle = { x: 700, y: 270, width: 400, height: 80 };
const teamEnter = { x: 1800, y: 1290, width: 302, height: 110 };
const storyZones = [
  zone("story-header", storyHeader), zone("story-fork-story", forkStory), zone("story-fork-combat", forkCombat),
  zone("story-acts", actBand), zone("story-chapter", chapterTitle), zone("story-team-enter", teamEnter),
];
// The fork-to-catalog transition is animated; poll a few newer frames, never re-click.
const confirmProbes = 4;
const probeDelayMs = 500;

export type StoryPage =
  | { readonly kind: "fork"; readonly x: number; readonly y: number }
  | { readonly kind: "acts" | "chapter" | "team" };

export function readStoryPage(scan: readonly Zone[]): StoryPage | null {
  const [top, story, combat, acts, chapter, enter] = scan;
  if (top?.id !== "story-header" || story?.id !== "story-fork-story" || combat?.id !== "story-fork-combat" ||
      acts?.id !== "story-acts" || chapter?.id !== "story-chapter" || enter?.id !== "story-team-enter") return null;
  const heading = confidentLines(top, storyHeader).map(line => plain(line.text));
  if (heading.includes("チーム編成")) {
    return confidentLines(enter, teamEnter).some(line => labelled(plain(line.text), "進入")) ? { kind: "team" } : null;
  }
  if (!heading.includes("ストーリー")) return null;
  const tiles = confidentLines(story, forkStory).filter(line => plain(line.text) === "ストーリー");
  const tile = tiles[0];
  if (tile && tiles.length === 1 && confidentLines(combat, forkCombat).some(line => plain(line.text) === "戦闘任務")) {
    return { kind: "fork", x: tile.bounds.x + tile.bounds.width / 2, y: tile.bounds.y + tile.bounds.height / 2 };
  }
  if (confidentLines(acts, actBand).some(line => /^第[0-9]+幕/u.test(plain(line.text)))) return { kind: "acts" };
  return confidentLines(chapter, chapterTitle).some(line => /^第[0-9]+章$/u.test(plain(line.text)))
    ? { kind: "chapter" } : null;
}

// Classifies only the given observation; the caller owns its freshness.
export function storyPage(ctx: MadoWorkflowContext, observation: MadoObservation): StoryPage | null {
  if (!calibrated(observation)) return null;
  return readStoryPage(ctx.call("scan_ocr_zones", { observation, basis, zones: storyZones }).zones);
}

function calibrated(observation: MadoObservation): boolean {
  return observation.width === basis.frame_width && observation.height === basis.frame_height;
}

function settleOnce(ctx: MadoWorkflowContext, id: string): MadoReceipt {
  try {
    return ctx.call("settle", { id });
  } finally {
    // Frees the sequence handle; it does not undo input.
    ctx.call("release", { id });
  }
}

// Observe through the menu animation without repeating the click.
export function confirmStoryPage(ctx: MadoWorkflowContext, before: MadoObservation, expected: "acts" | "fork"): boolean {
  try {
    for (let probe = 0; probe < confirmProbes; probe++) {
      ctx.call("wait", { duration_ms: probeDelayMs });
      const after = ctx.call("observe", {});
      try {
        if (!newer(before, after)) continue;
        if (storyPage(ctx, after)?.kind === expected) return true;
      } finally {
        ctx.call("release", { id: after.id });
      }
    }
    return false;
  } catch (error) {
    if ((error as MadoHostFault).category === "ScopeDeadline") return false;
    throw error;
  }
}

// Observed: replaying a cleared episode opened team setup, not dialogue.
const storyHandoffs = {
  acts: "Story: act catalog; choose an act manually.",
  chapter: "Story: chapter map; choose an episode manually.",
  team: "Story: team setup (チーム編成); choose the team and press 進入 manually.",
} as const;

// navigate is the profile opt-in for the single Story-tile click.
export function story(ctx: MadoWorkflowContext, navigate: boolean): Outcome {
  const before = ctx.call("observe", {});
  try {
    if (!calibrated(before)) return unsupportedGeometry;
    const page = storyPage(ctx, before);
    if (!page) return { kind: "handoff", page: null, reason: "Story: page not recognized; no input." };
    if (page.kind !== "fork") return { kind: "handoff", page: "story-" + page.kind, reason: storyHandoffs[page.kind] };
    if (!navigate) {
      return { kind: "handoff", page: "story-fork", reason: "Story: open the ストーリー tile manually; navigation is off." };
    }
    const sequence = ctx.call("submit", {
      observation: before,
      actions: [{ kind: "click", button: "left", x: page.x, y: page.y }],
    });
    const receipt = settleOnce(ctx, sequence.id);
    if (!submittedClick(receipt)) {
      return { kind: "handoff", page: "story-fork", reason: "Story: open-story click did not settle completely (" + receipt.status + "); not retried." };
    }
    return confirmStoryPage(ctx, before, "acts")
      ? { kind: "progressed", page: "story-acts", action: "Story: act catalog opened; choose an act manually." }
      : { kind: "handoff", page: "story-fork", reason: "Story: act catalog not confirmed after one click; not retried." };
  } finally {
    ctx.call("release", { id: before.id });
  }
}
