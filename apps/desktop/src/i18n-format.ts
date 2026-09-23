export function interpolate(template: string, values: Record<string, string | number>): string {
  return template.replace(/\{([A-Za-z][A-Za-z0-9]*)\}/g, (_, key: string) => {
    if (!Object.hasOwn(values, key)) throw new Error(`Missing message parameter: ${key}`);
    return String(values[key]);
  });
}
