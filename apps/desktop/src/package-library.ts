import type {InstalledRelease, ReleaseFacts} from './distribution.ts';
import type {PackageSource} from './types.ts';

// Local browsing of one installed-release listing. Everything here derives from the in-memory listing: nothing reads
// the host, selects a source or grants authority; selection always reverifies through the installed inspection.

export type LibraryFilter = 'all' | 'problems';
// One exact installed archive the host verified when the listing was read.
export interface LibraryVersion {release: InstalledRelease; facts: ReleaseFacts}
// Verified releases sharing one signing key and package ID. Display names never merge or split groups; `names` lists
// the distinct names of its versions in version order.
export interface LibraryGroup {id: string; key_id: string; package_id: string; names: string[]; versions: LibraryVersion[]}
// `problems` are entries without verified facts; they never join a trusted group.
export interface Library {groups: LibraryGroup[]; problems: InstalledRelease[]}
// What the operator is looking at; never a selection. `artifact` is the exact version chosen inside a group.
export type Highlight = {kind: 'group'; id: string; artifact: string | null} | {kind: 'problem'; package_id: string; artifact_id: string};
export type Resolved = {kind: 'group'; group: LibraryGroup; version: LibraryVersion | null} | {kind: 'problem'; release: InstalledRelease};
export type LibraryOutcome = 'unread' | 'loading' | 'failed' | 'empty' | 'noMatches' | 'noProblems' | 'shown';
export interface ArtifactRef {package_id: string; artifact_id: string}

// Code-unit order: locale independent and free of any version semantics.
function compare(a: string, b: string): number {
  return a < b ? -1 : a > b ? 1 : 0;
}

export function buildLibrary(releases: readonly InstalledRelease[]): Library {
  const groups = new Map<string, LibraryGroup>();
  const problems: InstalledRelease[] = [];
  for (const release of releases) {
    const facts = release.facts;
    if (facts === null || release.fault !== null) {
      problems.push(release);
      continue;
    }
    const id = JSON.stringify([facts.key_id, release.package_id]);
    let group = groups.get(id);
    if (!group) {
      group = {id, key_id: facts.key_id, package_id: release.package_id, names: [], versions: []};
      groups.set(id, group);
    }
    group.versions.push({release, facts});
  }
  // Authored version labels order lexically with the artifact as tie-breaker; nothing is treated as "latest".
  for (const group of groups.values()) {
    group.versions.sort((a, b) => compare(a.facts.release_version, b.facts.release_version) || compare(a.release.artifact_id, b.release.artifact_id));
    group.names = [...new Set(group.versions.map(item => item.facts.name))];
  }
  return {
    groups: [...groups.values()].sort((a, b) => compare(a.package_id, b.package_id) || compare(a.key_id, b.key_id)),
    problems: problems.sort((a, b) => compare(a.package_id, b.package_id) || compare(a.artifact_id, b.artifact_id)),
  };
}

function includes(value: string, needle: string): boolean {
  return value.toLowerCase().includes(needle);
}

// A group matches when any version matches by name, any localized game name or package ID, and keeps all of its
// versions. Problems match by their package ID only, because they carry no verified names.
export function filterLibrary(library: Library, query: string, filter: LibraryFilter): Library {
  const needle = query.trim().toLowerCase();
  if (needle === '') return filter === 'all' ? library : {groups: [], problems: library.problems};
  return {
    groups: filter === 'all' ? library.groups.filter(group => group.versions.some(({facts}) => includes(facts.name, needle)
      || includes(facts.package_id, needle) || Object.values(facts.game_names).some(name => includes(name, needle)))) : [],
    problems: library.problems.filter(release => includes(release.package_id, needle)),
  };
}

// Failure, an unread listing, an empty installation and an empty search stay distinct outcomes.
export function libraryOutcome(releases: readonly InstalledRelease[] | null, error: unknown, loading: boolean, shown: Library, query: string): LibraryOutcome {
  if (releases === null) return loading ? 'loading' : error !== null ? 'failed' : 'unread';
  if (releases.length === 0) return 'empty';
  if (shown.groups.length > 0 || shown.problems.length > 0) return 'shown';
  return query.trim() === '' ? 'noProblems' : 'noMatches';
}

// The installed artifact a saved source names; a directory stays a directory even under the packages folder.
export function savedArtifact(saved: PackageSource | null): ArtifactRef | null {
  return saved?.kind === 'installed' ? {package_id: saved.package_id, artifact_id: saved.artifact_id} : null;
}

export function sameArtifact(a: ArtifactRef | null, b: ArtifactRef | null): boolean {
  return a !== null && b !== null && a.package_id === b.package_id && a.artifact_id === b.artifact_id;
}

// Where an artifact appears in a listing: a verified version, a problem entry, or nowhere.
export function locate(library: Library, artifact: ArtifactRef): Resolved | null {
  for (const group of library.groups) {
    const version = group.versions.find(item => sameArtifact(item.release, artifact));
    if (version) return {kind: 'group', group, version};
  }
  const release = library.problems.find(item => sameArtifact(item, artifact));
  return release ? {kind: 'problem', release} : null;
}

// The version a newly highlighted group shows: the saved artifact, else its only version, else none until chosen.
export function initialArtifact(group: LibraryGroup, saved: ArtifactRef | null): string | null {
  const current = group.versions.find(item => sameArtifact(item.release, saved));
  if (current) return current.release.artifact_id;
  return group.versions.length === 1 ? group.versions[0].release.artifact_id : null;
}

// Display an exact highlighted/saved/sole version, never an inferred latest. An unchosen group summarizes all authors.
export function groupSummary(group: LibraryGroup, saved: ArtifactRef | null, highlight: Highlight | null): {authors: (string | null)[]; version: LibraryVersion | null} {
  const artifact = highlight?.kind === 'group' && highlight.id === group.id ? highlight.artifact : initialArtifact(group, saved);
  const version = group.versions.find(item => item.release.artifact_id === artifact) ?? null;
  const authors = [...new Set((version ? [version] : group.versions).map(item => item.facts.author_name.trim() || null))];
  return {authors, version};
}

// Resolves a highlight against the shown listing. A highlight hidden by search or gone after refresh resolves to
// nothing rather than to another release; a vanished version leaves its group without a chosen version.
export function resolveHighlight(shown: Library, highlight: Highlight | null): Resolved | null {
  if (highlight === null) return null;
  if (highlight.kind === 'problem') {
    const release = shown.problems.find(item => sameArtifact(item, highlight));
    return release ? {kind: 'problem', release} : null;
  }
  const group = shown.groups.find(item => item.id === highlight.id);
  if (!group) return null;
  return {kind: 'group', group, version: group.versions.find(item => item.release.artifact_id === highlight.artifact) ?? null};
}
