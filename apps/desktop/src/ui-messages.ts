import enData from './locales/ui.en.json' with {type: 'json'};
import jaData from './locales/ui.ja.json' with {type: 'json'};
import {interpolate} from './i18n-format.ts';

function known(labels: Record<string, string>, value: string, key = value): string {
  return Object.hasOwn(labels, key) ? labels[key] : value;
}

function bounds(min: number | undefined, max: number | undefined) {
  return min === undefined ? max === undefined ? 'none' : 'maximum' : max === undefined ? 'minimum' : 'both';
}

function catalog(data: typeof enData) {
  return {
    phase: (value: string) => known(data.phase, value),
    operation: (value: string) => known(data.operation, value),
    lane: (value: string) => known(data.lane, value),
    severity: (value: string) => known(data.severity, value, value.toLowerCase()),
    entryOutcome: (value: string) => known(data.entryOutcome, value),
    common: {
      ...data.common,
      revision: (label: string, revision: number) => interpolate(data.common.revision, {label, revision}),
    },
    settings: {
      ...data.settings,
      invalid: (count: number, categories: string) => interpolate(data.settings.invalid[categories ? count === 1 ? 'oneCategories' : 'otherCategories' : count === 1 ? 'one' : 'other'], {count, categories}),
      invalidCount: (count: number) => interpolate(data.settings.invalidCount, {count}),
      wait: (reason: string) => interpolate(data.settings.wait, {reason}),
      cards: (count: number) => interpolate(data.settings.cards[count === 1 ? 'one' : 'other'], {count}),
      seconds: (count: number) => interpolate(data.settings.seconds[count === 1 ? 'one' : 'other'], {count}),
      logLimitHelp: (limit: number | null) => interpolate(data.settings.logLimitHelp, {limit: limit ?? data.common.notLoaded}),
    },
    environment: {
      ...data.environment,
      truncated: (count: number) => interpolate(data.environment.truncated[count === 1 ? 'one' : 'other'], {count}),
      staleHelp: (reasons: string[]) => interpolate(data.environment.staleHelp, {reasons: reasons.join(data.environment.reasonSeparator)}),
      wait: (reason: string) => interpolate(data.environment.wait, {reason}),
      unsupported: (profile: string) => interpolate(data.environment.unsupported, {profile}),
      profileLabel: (profile: keyof typeof data.environment.profileLabels) => data.environment.profileLabels[profile],
      mismatch: (model: string, language: string, provider: string, runtime: string) => interpolate(data.environment.mismatch, {model, language, provider, runtime}),
    },
    run: {
      ...data.run,
      heading: (label: string) => interpolate(data.run.heading, {label}),
      owned: (operation: string | null) => interpolate(data.run.owned, {operation: operation ?? data.common.none}),
      settled: (operation: string | null) => interpolate(data.run.settled, {operation: operation ?? data.common.none}),
      inspected: (revision: number) => interpolate(data.run.inspected, {revision}),
      descriptorHelp: (limit: number) => interpolate(data.run.descriptorHelp, {limit}),
      startUses: (profile: string | null, replay: boolean, environment: string | null, descriptor: string | null) => interpolate(data.run.startUses[replay ? profile === null ? 'replayDraft' : 'replayProfile' : profile === null ? 'draft' : 'profile'], {profile: profile ?? '', environment: environment ?? data.run.noSavedEnvironment, descriptor: descriptor ?? data.common.none}),
      olderRevision: (old: number, current: number) => interpolate(data.run.olderRevision, {old, current}),
      stopTarget: (operation: string | null) => interpolate(data.run.stopTarget, {operation: operation ?? data.common.none}),
      runKind: (lane: string) => interpolate(data.run.runKind, {lane}),
    },
    result: {
      ...data.result,
      truncated: (count: number) => interpolate(data.result.truncated[count === 1 ? 'one' : 'other'], {count}),
      disclosureHelp: (privateDetail: boolean, kib: number) => interpolate(data.result.disclosureHelp[privateDetail ? 'private' : 'ordinary'], {kib}),
    },
    schema: {
      ...data.schema,
      fieldAction: (present: boolean, path: string) => interpolate(data.schema.fieldAction[present ? 'omit' : 'add'], {path}),
      omittedDefault: (value: string) => interpolate(data.schema.omittedDefault, {value}),
      unknown: (path: string, value: string) => interpolate(data.schema.unknown, {path, value}),
      ordered: (count: number, min: number | undefined, max: number | undefined) => interpolate(data.schema.ordered[bounds(min, max)][count === 1 ? 'one' : 'other'], {count, min: min ?? '', max: max ?? ''}),
      moveUp: (path: string) => interpolate(data.schema.moveUp, {path}),
      moveDown: (path: string) => interpolate(data.schema.moveDown, {path}),
      removeItem: (path: string) => interpolate(data.schema.removeItem, {path}),
      invalid: (value: string) => interpolate(data.schema.invalid, {value}),
      length: (min: number, max: number | undefined) => interpolate(data.schema.length[max === undefined ? 'unbounded' : 'bounded'], {min, max: max ?? ''}),
      numeric: (type: string, min: number | undefined, max: number | undefined) => interpolate(data.schema.numeric[bounds(min, max)], {type, min: min ?? '', max: max ?? ''}),
      mismatch: (type: string, value: string) => interpolate(data.schema.mismatch, {type, value}),
      replace: (type: string) => interpolate(data.schema.replace, {type}),
    },
    logs: {
      ...data.logs,
      missing: (sequence: number) => interpolate(data.logs.missing, {sequence}),
      filtered: (count: number) => interpolate(data.logs.filtered[count === 1 ? 'one' : 'other'], {count}),
      stats: (shown: number, scoped: number, retained: number, limit: number) => interpolate(data.logs.stats, {shown, scoped, retained, limit}),
      fileError: (error: string) => interpolate(data.logs.fileError, {error}),
      lossHelp: (loss: boolean) => data.logs.lossHelp[loss ? 'loss' : 'none'],
    },
    notifications: {
      ...data.notifications,
      dismiss: (id: number) => interpolate(data.notifications.dismiss, {id}),
      event: (id: number) => interpolate(data.notifications.event, {id}),
    },
    workspaces: {
      ...data.workspaces,
      attention: (count: number) => interpolate(data.workspaces.attention[count === 1 ? 'one' : 'other'], {count}),
      label: (name: string) => interpolate(data.workspaces.label, {name}),
      open: (count: number) => interpolate(data.workspaces.open, {count}),
      closeLabel: (name: string) => interpolate(data.workspaces.closeLabel, {name}),
      closeTitle: (name: string) => interpolate(data.workspaces.closeTitle, {name}),
    },
    bootstrap: {
      ...data.bootstrap,
      state: (value: string) => known(data.bootstrap.state, value),
      legacyHelp: (path: string) => interpolate(data.bootstrap.legacyHelp, {path}),
      block: (kind: keyof typeof data.bootstrap.block) => data.bootstrap.block[kind],
    },
    create: {
      ...data.create,
      count: (count: number, limit: number) => interpolate(data.create.count, {count, limit}),
      openLimit: (limit: number) => interpolate(data.create.openLimit, {limit}),
      savedLimit: (limit: number) => interpolate(data.create.savedLimit, {limit}),
      errors: (kind: keyof typeof data.create.errors) => data.create.errors[kind],
    },
    reopen: {
      ...data.reopen,
      directory: (id: string) => interpolate(data.reopen.directory, {id}),
      archive: (id: string) => interpolate(data.reopen.archive, {id}),
      references: (count: number) => interpolate(data.reopen.references, {count}),
      saved: (count: number, limit: number) => interpolate(data.reopen.saved, {count, limit}),
      limit: (limit: number) => interpolate(data.reopen.limit, {limit}),
      reopenLabel: (name: string) => interpolate(data.reopen.reopenLabel, {name}),
    },
    guidance: data.guidance,
  };
}

const en = catalog(enData);
const ja: typeof en = catalog(jaData);
export const uiMessages = {en, ja};
