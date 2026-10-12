// LNG_KEY: the shared language-key convention for localized package metadata. Keys are standard i18n language tags
// (`en`, `ja`, `zh-Hant`, `pt-BR`), not a package-specific code scheme, and the application's own translation locales
// are only a few of them. The vocabulary is the Unicode language identifier subset of BCP 47 that the host validates
// with ICU4X: a 2–3 letter language, an optional script, an optional region and variants, without extensions,
// private use or alias replacement. The canonical form is ICU4X's normalization: language lowercase, script title
// case, region uppercase, variants lowercase in ASCII order. A saved key must already be canonical; the editor
// normalizes only a key the author deliberately commits.

export const LANGUAGE_KEY_BYTES = 64;

const LANGUAGE = /^[A-Za-z]{2,3}$/;
const SCRIPT = /^[A-Za-z]{4}$/;
const REGION = /^(?:[A-Za-z]{2}|[0-9]{3})$/;
const VARIANT = /^(?:[A-Za-z0-9]{5,8}|[0-9][A-Za-z0-9]{3})$/;

// The canonical key for `text`, or null when it is not a language key. `_` separators are accepted as typed input
// and written as `-`; the result is always a key the host accepts unchanged.
export function canonicalLanguageKey(text: string): string | null {
  if (text.length === 0 || text.length > LANGUAGE_KEY_BYTES || !/^[\x21-\x7e]+$/.test(text)) return null;
  const subtags = text.split(/[-_]/);
  const language = subtags.shift()!;
  if (!LANGUAGE.test(language)) return null;
  const parts = [language.toLowerCase()];
  if (subtags.length > 0 && SCRIPT.test(subtags[0])) {
    const script = subtags.shift()!;
    parts.push(script[0].toUpperCase() + script.slice(1).toLowerCase());
  }
  if (subtags.length > 0 && REGION.test(subtags[0])) parts.push(subtags.shift()!.toUpperCase());
  const variants = subtags.map(subtag => subtag.toLowerCase());
  if (!variants.every(variant => VARIANT.test(variant)) || new Set(variants).size !== variants.length) return null;
  // ASCII order, independent of the WebView's collation.
  variants.sort((left, right) => left < right ? -1 : left > right ? 1 : 0);
  return [...parts, ...variants].join('-');
}

// What a saved localized-name key must be.
export function languageKeyValid(key: string): boolean {
  return canonicalLanguageKey(key) === key;
}

export interface LanguageCandidate {key: string; label: string}

// Suggestions from the i18n-owned candidate labels (keys in resource order), filtered by what the author typed:
// keys starting with it first, then candidates whose label in the current UI language contains it. The list only
// suggests; any valid key may be typed instead.
export function languageCandidates(labels: Readonly<Record<string, string>>, query: string): LanguageCandidate[] {
  const all = Object.entries(labels).map(([key, label]) => ({key, label}));
  const text = query.trim().toLowerCase().replaceAll('_', '-');
  if (text === '') return all;
  const prefixed = all.filter(item => item.key.toLowerCase().startsWith(text));
  return [...prefixed, ...all.filter(item => !prefixed.includes(item) && item.label.toLowerCase().includes(text))];
}
