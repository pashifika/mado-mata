export const methods: Readonly<Record<string, readonly [args: string, result: string, documentation: string]>>;
export function definitions(schema: unknown): string;
export function unknownOptionsDefinitions(): string;
