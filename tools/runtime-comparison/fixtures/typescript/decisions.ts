import { choose } from "@mado/helper";

type Decision = MadoOptions["priorities"][number];

export function decide(template: MadoRecognition | null, ocr: MadoRecognition | null): Decision | null {
  const threshold = host.options.recognition.threshold;
  const selected = choose(host.options.priorities, {
    template: template !== null && template.score >= threshold,
    ocr: ocr !== null && ocr.score >= threshold,
  });
  if (selected !== null && selected !== "template" && selected !== "ocr") {
    throw new Error("Approved helper returned an unknown decision");
  }
  return selected;
}

export function fail(): never {
  throw new Error("internal helper failure");
}
