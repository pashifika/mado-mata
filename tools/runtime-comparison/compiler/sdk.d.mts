/** Host admission stage: module evaluation, an entry, or after ordinary admission closes. */
export type SdkStage = "module" | "readiness" | "workflow" | "closed";
/** desktop-native is the reviewed Desktop Native subset of native; native methods also apply there under their stage/authority rules. */
export type SdkLane = "controlled" | "replay" | "native" | "desktop-native";
/** Native authority required in native lanes; controlled and replay runs grant none. */
export type SdkAuthority = "none" | "capture" | "input" | "target";
export interface SdkPurpose { readonly en: readonly string[]; readonly ja: readonly string[] }
export interface SdkAvailability {
  readonly stages: readonly SdkStage[]; readonly lanes: readonly SdkLane[]; readonly authority: SdkAuthority;
}
export interface SdkGuidance {
  readonly purpose: SdkPurpose; readonly constraints: readonly string[];
  readonly availability: SdkAvailability; readonly examples: readonly string[];
}
export const methods: Readonly<Record<string, readonly [args: string, result: string, documentation: string, guidance: SdkGuidance]>>;
export function definitions(schema: unknown): string;
export function unknownOptionsDefinitions(): string;

/** Installed SDK contract; `revision` changes with any signature, guidance or example change. */
export interface SdkIdentity { readonly contract: "mado-host-v1"; readonly revision: string; readonly methods: number }
export const sdk: SdkIdentity;
export const SDK_SEARCH_LIMITS: Readonly<{ defaultLimit: number; maxLimit: number; queryUnits: number; responseBytes: number }>;

export interface SdkError {
  code: "sdk_query_invalid" | "sdk_cursor_invalid" | "sdk_options_invalid" | "sdk_name_invalid";
  message: string;
}
export type SdkResponse<T> = { ok: true; result: T } | { ok: false; error: SdkError };
export interface SdkMatch {
  name: string; args: string; result: string; description: string;
  /** exact name, name token, documented purpose term, or a blank query listing every method. */
  match: "exact" | "name" | "purpose" | "all";
  /** Documented names/terms that matched, for explanation only. */
  terms: string[];
}
export interface SdkSearchResult {
  sdk: SdkIdentity; query: string; total: number; offset: number; matches: SdkMatch[];
  noMatch: boolean;
  /** Opaque continuation for the same query in the same SDK revision; null on the last page. */
  next: string | null;
  complete: boolean;
}
export interface SdkExample { id: string; title: string; source: string }
export interface SdkMethodDetail {
  name: string; args: string; result: string; description: string;
  purpose: { en: string[]; ja: string[] };
  constraints: string[];
  /** `declared` is static compiler/completion support, not permission or readiness to call. */
  availability: { declared: true; stages: SdkStage[]; lanes: SdkLane[]; authority: SdkAuthority };
  examples: SdkExample[];
}
export type SdkDetailResult = { sdk: SdkIdentity; name: string; rules: string[] } &
  ({ noMatch: false; method: SdkMethodDetail } | { noMatch: true; method: null });
export function searchSdk(query: string, options?: { cursor?: string; limit?: number }): SdkResponse<SdkSearchResult>;
export function sdkDetail(name: string): SdkResponse<SdkDetailResult>;
