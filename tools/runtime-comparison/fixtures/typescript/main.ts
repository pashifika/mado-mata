import { decide } from "./decisions.js";

export function readiness(): MadoReady {
  const observation = host.call("observe", {});
  host.call("release", { id: observation.id });
  return "Ready";
}

export function workflow(): void {
  const observation = host.call("observe", {});
  const asset = host.call("asset", { id: "marker" });
  const roi = host.options.recognition.roi;
  const template = host.call("recognize", { observation, kind: "template", asset: asset.id, roi });
  const ocr = host.call("recognize", { observation, kind: "ocr", roi });
  const decision = decide(template, ocr);
  host.state.decision = decision;
  host.state.count = (typeof host.state.count === "number" ? host.state.count : 0) + 1;
  if (decision !== null) {
    const key = host.options.actions[decision];
    const submission = host.call("submit", {
      observation,
      actions: [{ kind: "key_down", key }, { kind: "key_up", key }],
    });
    const receipt = host.call("settle", { id: submission.id });
    host.state.receipt = receipt;
    const after = host.call("observe", {});
    host.state.postcondition = host.call("postcondition", {
      observation: after, checkpoint: observation, expected: host.options.postcondition,
    });
    host.call("release", { id: after.id });
    host.call("release", { id: receipt.id });
  }
  if (template !== null) host.call("release", { id: template.id });
  if (ocr !== null) host.call("release", { id: ocr.id });
  host.call("release", { id: observation.id });
  host.call("log", { message: decision === null ? "no-match" : decision });
}
