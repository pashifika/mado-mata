import { openStory } from "./home.js";
import { recommendRemoval, story, type Outcome } from "./progression.js";
import { NO_HAND_HUD, sortie } from "./sortie.js";
// Readiness establishes visible game content, not authority to act on a page.
const readyBasis = { frame_width: 2102, frame_height: 1640, content: { x: 0, y: 0, width: 2102, height: 1640 } };
const readyZones = [
  { id: "heading", region: { u0: 0.05, v0: 0.16, u1: 0.58, v1: 0.23 } },
  { id: "home", region: { u0: 0.75, v0: 0.24, u1: 0.95, v1: 0.31 } },
  { id: "hand", region: { u0: 996 / 2102, v0: 1391 / 1640, u1: 1147 / 2102, v1: 1435 / 1640 } },
];

function visibleGame(observation: MadoObservation): boolean {
  const scan = host.call("scan_ocr_zones", { observation, basis: readyBasis, zones: readyZones });
  return scan.zones.some(zone => zone.regions.some(line => {
    if (line.confidence < 0.9) return false;
    const text = line.text.normalize("NFKC").replace(/\s+/gu, "");
    if (zone.id === "hand") return /^(?:0?[0-9]|10)\/10$/.test(text);
    if (zone.id === "home") return text === "ストーリー";
    return ["ストーリー", "チーム編成"].includes(text) || /^排除するカードを[0-9]+枚選択してください。?$/.test(text);
  }));
}

export function readiness(): MadoReady {
  if (host.options.desktopStartup) {
    let target = host.call("target_start", {});
    for (let i = 0; i < 100 && target.status !== "capture_ready"; i++) {
      host.call("wait", { duration_ms: 500 });
      target = host.call("target_status", {});
    }
    if (target.status !== "capture_ready") throw new Error("SSR capture is not ready.");
    const observation = host.call("observe", {});
    try {
      if (observation.width !== 2102 || observation.height !== 1640) {
        throw new Error("SSR window does not match the calibrated layout.");
      }
      if (!visibleGame(observation)) throw new Error("No supported SSR screen marker is visible.");
    } finally { host.call("release", { id: observation.id }); }
  }
  return "Ready";
}

function decision(ctx: MadoWorkflowContext, checkHome: boolean): Outcome {
  const options = host.options;
  if (options.mode === "story") {
    return (checkHome ? openStory(ctx, options.navigateStory) : null) ?? story(ctx, options.navigateStory);
  }
  if (options.mode === "sortie") {
    const result = sortie(ctx, options.cardPriority, options.excludedCards, options.playMagneticField);
    if (result !== NO_HAND_HUD) return { kind: "handoff", page: null, reason: result };
  }
  return recommendRemoval(ctx, options.removal) ?? {
    kind: "handoff", page: null, reason: "No supported progression page was identified; no input.",
  };
}

export function workflow(): void {
  const root = host.call("context", {});
  let checkHome = true;
  for (;;) {
    const step = root.child({ timeout_ms: 20000 });
    try {
      const result = decision(step, checkHome);
      host.call("log", { message: result.kind === "handoff" ? result.reason : result.action });
      if (result.kind === "handoff" || result.page === "story-acts") return;
      checkHome = false;
    } catch (error) {
      if ((error as MadoHostFault).category !== "ScopeDeadline") throw error;
      host.call("log", { message: "Decision timed out; inspect the last receipt and scene. No retry." });
      return;
    } finally { step.close(); }
  }
}
