import en from './locales/app.en.json' with {type: 'json'};
import ja from './locales/app.ja.json' with {type: 'json'};
import {uiMessages} from './ui-messages.ts';
import {interpolate} from './i18n-format.ts';

export type Locale = 'en' | 'ja';
export type Command = "loadingBootstrap" | "initializing" | "retrying" | "importingRoot" | "restoring" | "recovering" | "refreshingStatus" | "snapshotting"
  | "creatingWorkspace" | "reopeningWorkspace" | "loadingCatalog" | "inspectingPackage" | "validatingDraft" | "savingProfile" | "renamingProfile"
  | "deletingProfile" | "importingProfiles" | "reinspectingPackage" | "admittingRun" | "admittingCheck" | "savingSettings" | "closingWorkspace";

function appMessages(copy: typeof en) {
  return {...copy.app,
    workspaceLimit: (limit: number) => interpolate(copy.app.workspaceLimit, {limit}),
    closeConfirm: (name: string) => interpolate(copy.app.closeConfirm, {name}),
    workspaceAria: (name: string) => interpolate(copy.app.workspaceAria, {name}),
    activity: (name: string) => interpolate(copy.app.activity, {name}),
    closedLabel: (name: string) => interpolate(copy.app.closedLabel, {name}),
    unknownOrigin: (name: string) => interpolate(copy.app.unknownOrigin, {name}),
    retainedOutcome: (revision: number) => interpolate(copy.app.retainedOutcome, {revision}),
    attention: (reason: string) => interpolate(copy.app.attention, {reason}),
    ready: (name: string) => interpolate(copy.app.ready, {name}),
    descriptorLimit: (limit: number) => interpolate(copy.app.descriptorLimit, {limit}),
    profileSaved: (name: string, id: string) => interpolate(copy.app.profileSaved, {name, id}),
    profileRenamed: (name: string) => interpolate(copy.app.profileRenamed, {name}),
    profilesImported: (imported: number, unchanged: number) => interpolate(copy.app.profilesImported, {imported, unchanged}),
    profilesImportPartial: (imported: number, unchanged: number) => interpolate(copy.app.profilesImportPartial, {imported, unchanged}),
    deletedElsewhere: (name: string) => interpolate(copy.app.deletedElsewhere, {name}),
    updatedElsewhere: (name: string) => interpolate(copy.app.updatedElsewhere, {name}),
    renamedElsewhere: (name: string) => interpolate(copy.app.renamedElsewhere, {name}),
    stopFailed: (diagnostic: string) => interpolate(copy.app.stopFailed, {diagnostic}),
    settingsSaved: (profile: string) => interpolate(copy.app.settingsSaved, {profile}),
    waitForCommand: (command: Command) => interpolate(copy.app.waitForCommand, {command: copy.app[command]}),
  };
}

export const messages = {
  en: {app: appMessages(en), validation: en.validation, ui: uiMessages.en},
  ja: {app: appMessages(ja), validation: ja.validation, ui: uiMessages.ja},
};

type AppMessages = typeof messages.en.app;
export type Message = {[Key in keyof AppMessages]: AppMessages[Key] extends (...args: infer Args) => string
  ? {key: Key; args: Args} : {key: Key}}[keyof AppMessages];

export function renderMessage(locale: Locale, message: Message | null): string {
  if (message === null) return '';
  const entry = messages[locale].app[message.key];
  // Message ties each key to its parameter tuple; indexing loses that correlation.
  return typeof entry === 'string' ? entry : (entry as (...args: (string | number)[]) => string)(...('args' in message ? message.args : []));
}

export class LocalFault extends Error {
  readonly category = 'Draft';
  readonly context = null;
  readonly presentation: Message;
  constructor(presentation: Message) {
    super('LocalFault');
    this.presentation = presentation;
  }
}

const timeFormats = {en: new Intl.DateTimeFormat('en', {timeStyle: 'medium'}), ja: new Intl.DateTimeFormat('ja', {timeStyle: 'medium'})};
export function formatTime(locale: Locale, timeMs: number): string {
  return timeFormats[locale].format(timeMs);
}
