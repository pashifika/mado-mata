export function line(text, x, y, width, height) {
  return { text, confidence: 0.99, bounds: { x, y, width, height },
    geometry: [[x, y], [x + width, y], [x + width, y + height], [x, y + height]] };
}

export function hand(...additional) {
  return [
    { id: "cards", outcome: "recognized", regions: [
      line("Alpha", 520, 1160, 110, 40), line("基本攻撃", 520, 1185, 100, 35),
      line("Beta", 760, 1140, 160, 36), line("基本スキル", 760, 1168, 116, 32), ...additional,
    ] },
    { id: "hand-count", outcome: "recognized", regions: [line("02/10", 1001, 1394, 100, 32)] },
    { id: "ap", outcome: "recognized", regions: [line("2", 1028, 1338, 40, 48)] },
  ];
}
