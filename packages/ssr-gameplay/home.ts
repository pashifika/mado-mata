import { storyPage, type Outcome } from "./progression.js";
import { newer, submittedClick } from "./screen.js";

const width = 2102, height = 1640;
const basis: MadoRecognitionBasis = { frame_width: width, frame_height: height,
  content: { x: 0, y: 0, width, height } };

function area(id: string, x: number, y: number, w: number, h: number) {
  return { id, region: { u0: x / width, v0: y / height, u1: (x + w) / width, v1: (y + h) / height } };
}
const menu = [area("story", 1630, 395, 330, 95), area("chaos", 1755, 738, 190, 87)];

type Zone = MadoOcrZoneScanResult["zones"][number];
function exact(zone: Zone | undefined, label: string) {
  const lines = zone?.regions.filter(r => r.text === label);
  const match = lines?.[0];
  return match && lines?.length === 1 && match.confidence >= 0.9 ? match.bounds : null;
}

export function openStory(ctx: MadoWorkflowContext, navigate: boolean): Outcome | null {
  const before = ctx.call("observe", {});
  try {
    if (before.width !== width || before.height !== height) return null;
    const [entry, sibling] = ctx.call("scan_ocr_zones", { observation: before, basis, zones: menu }).zones;
    const title = exact(entry, "ストーリー");
    if (!title || !exact(sibling, "カオス")) return null;
    const handoff = (reason: string): Outcome => ({ kind: "handoff", page: "home", reason });
    if (!navigate) return handoff("Home: Story is available; navigation is off.");
    const x = title.x + title.width / 2, y = title.y + title.height / 2;
    if (x < 1630 || x >= 1960 || y < 395 || y >= 490) return handoff("Home: Story label is outside its control.");
    const sequence = ctx.call("submit", { observation: before, actions: [{ kind: "click", button: "left", x, y }] });
    try {
      const receipt = ctx.call("settle", { id: sequence.id });
      if (!submittedClick(receipt)) {
        return handoff("Home: Story click did not settle completely; no retry.");
      }
    } finally { ctx.call("release", { id: sequence.id }); }
    ctx.call("wait", { duration_ms: 800 });
    const after = ctx.call("observe", {});
    try {
      if (newer(before, after) && storyPage(ctx, after)?.kind === "fork") {
        return { kind: "progressed", page: "story-fork", action: "opened Story from home" };
      }
      return handoff("Home: Story fork not confirmed on a newer frame; no retry.");
    } finally { ctx.call("release", { id: after.id }); }
  } finally { ctx.call("release", { id: before.id }); }
}
