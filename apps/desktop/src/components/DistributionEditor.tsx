import {useLayoutEffect, useRef} from 'react';
import LanguageKeyInput from './LanguageKeyInput.tsx';
import {RebuildControl, TextField} from './MetadataControls.tsx';
import {HelpTrigger} from './ContextualHelp.tsx';
import {DISTRIBUTION_ENTRIES, DISTRIBUTION_TEXT_BYTES, aliasIssue, distributionTextIssue, isObject, nameRowIssues, readDistribution, repairDistribution,
  withAliases, withDistribution, withDistributionText, withNameRows} from '../metadata.ts';
import type {DistributionPart, JsonObject, NameRow} from '../metadata.ts';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';

interface Props {manifest: JsonObject; onChange: (next: JsonObject) => void}

// Release and search metadata in the manifest's `distribution` member. Every control edits the manifest draft itself,
// so incomplete, invalid and conflicting input lives in the selected file's text and history (Undo, Redo, Discard,
// Save) like any other form edit; nothing is held aside. Internal tags are never shown and are carried over unchanged.
export default function DistributionEditor({manifest, onChange}: Props) {
  const r = messages[useLocale()].ui.releaseMetadata;
  // The control to focus once an added or removed row is rendered from the draft.
  const focus = useRef<string | null>(null);
  useLayoutEffect(() => {
    if (focus.current === null) return;
    document.getElementById(focus.current)?.focus();
    focus.current = null;
  });
  const distribution = manifest.distribution;
  const remove = <RebuildControl id="authoring-distribution-remove" label={r.remove} question={r.removeConfirm} confirm={r.removeConfirmed} disabled={false}
    onConfirm={() => onChange(withDistribution(manifest, false))}/>;

  let body;
  if (distribution === undefined) {
    body = <><p className="muted">{r.absent}</p>
      <div className="button-row"><button id="authoring-distribution-add" type="button" onClick={() => {
        focus.current = 'authoring-distribution-release-version';
        onChange(withDistribution(manifest, true));
      }}>{r.add}</button></div></>;
  } else if (!isObject(distribution)) {
    body = <><p id="authoring-distribution-invalid" className="inline-warning">{r.notObject}</p><div className="button-row">{remove}</div></>;
  } else {
    const view = readDistribution(distribution);
    const text = (part: DistributionPart, id: string, label: string, help?: string) => {
      const issue = distributionTextIssue(part, view.text[part]);
      const error = issue === null ? null : issue === 'required' ? r.required : issue === 'length' && part !== 'game_id' ? r.textLength(DISTRIBUTION_TEXT_BYTES[part])
        : part === 'game_id' ? r.gameIdInvalid : r.versionInvalid;
      return <TextField id={id} label={label} value={view.text[part]} help={help} error={error} onChange={next => onChange(withDistributionText(manifest, part, next))}/>;
    };
    const descriptionIssue = distributionTextIssue('description', view.text.description);
    const game = view.game;
    body = <>
      {view.issues.length > 0 && <ul id="authoring-distribution-issues" className="schema-issues">{view.issues.map(issue =>
        <li key={`${issue.code}:${'key' in issue ? issue.key : ''}`} className="inline-warning" data-issue={issue.code}>{r.issue(issue)}
          <button type="button" data-repair={issue.code} onClick={() => onChange(repairDistribution(manifest, issue))}>{r.repair(issue)}</button></li>)}</ul>}
      <div className="two-col">
        {text('release_version', 'authoring-distribution-release-version', r.releaseVersion, r.releaseVersionHelp)}
        {text('name', 'authoring-distribution-name', r.name)}
      </div>
      <div className="two-col">
        {text('author_name', 'authoring-distribution-author-name', r.authorName)}
        {text('license', 'authoring-distribution-license', r.license, r.licenseHelp)}
      </div>
      <div className="field"><label htmlFor="authoring-distribution-description">{r.description}</label>
        <textarea id="authoring-distribution-description" rows={3} value={view.text.description} aria-invalid={descriptionIssue !== null}
          aria-describedby={`authoring-distribution-description-help${descriptionIssue ? ' authoring-distribution-description-error' : ''}`}
          onChange={event => onChange(withDistributionText(manifest, 'description', event.target.value))}/>
        <p id="authoring-distribution-description-help" className="field-help">{r.descriptionHelp}</p>
        {descriptionIssue && <p id="authoring-distribution-description-error" className="field-error">{r.textLength(DISTRIBUTION_TEXT_BYTES.description)}</p>}</div>
      {game && <>
        {text('game_id', 'authoring-distribution-game-id', r.gameId, r.gameIdHelp)}
        {game.names && <GameNames rows={game.names} pairs={view.pairs} focus={focus} onChange={rows => onChange(withNameRows(manifest, rows))}/>}
        {game.aliases && <Aliases aliases={game.aliases} focus={focus} onChange={aliases => onChange(withAliases(manifest, aliases))}/>}
      </>}
      <div className="button-row">{remove}</div>
    </>;
  }
  return <fieldset id="authoring-distribution" className="metadata-group distribution-form"><legend>{r.heading}</legend>
    <p className="field-help">{r.help}</p>
    {body}
  </fieldset>;
}

type Focus = {current: string | null};

// Localized game names: one joined row per language, the language-key combobox on the left and the name on the
// right. Rows are positional; conflicting or incomplete rows stay in the draft exactly as entered.
function GameNames({rows, pairs, focus, onChange}: {rows: NameRow[]; pairs: boolean; focus: Focus; onChange: (rows: NameRow[]) => void}) {
  const r = messages[useLocale()].ui.releaseMetadata;
  const issues = nameRowIssues(rows);
  const set = (index: number, row: NameRow) => onChange(rows.map((item, at) => at === index ? row : item));
  const groupError = rows.length === 0 ? r.namesEmpty : rows.length > DISTRIBUTION_ENTRIES ? r.namesLimit(DISTRIBUTION_ENTRIES) : null;
  return <fieldset id="authoring-distribution-names" className="metadata-group distribution-list"
    aria-describedby={`authoring-distribution-names-help${groupError ? ' authoring-distribution-names-error' : ''}`}>
    <legend>{r.gameNames}</legend>
    <p id="authoring-distribution-names-help" className="field-help">{r.gameNamesHelp}</p>
    {pairs && <p id="authoring-game-names-pairs" className="inline-warning">{r.pairs}</p>}
    {rows.length > 0 && <div className="distribution-rows">{rows.map((row, index) => {
      const keyId = `authoring-game-name-${index}-key`;
      const nameId = `authoring-game-name-${index}-name`;
      const issue = issues[index];
      const keyError = issue.key === null ? null : r.keyIssue(issue.key);
      const nameError = issue.name === null ? null : r.nameIssue[issue.name];
      return <div key={index} className="distribution-row" role="group" aria-label={r.nameRow(index + 1)} data-row={index}>
        <div className="distribution-row-controls">
          <div className="input-select-group language-name-group">
            <label htmlFor={keyId} className="visually-hidden">{r.languageKey}</label>
            <LanguageKeyInput id={keyId} value={row.key} placeholder={r.languageKey} invalid={keyError !== null}
              describedBy={keyError === null ? undefined : `${keyId}-error`} onChange={key => set(index, {...row, key})}/>
            <label htmlFor={nameId} className="visually-hidden">{r.gameName}</label>
            <input id={nameId} type="text" value={row.name} placeholder={r.gameName} aria-invalid={nameError !== null}
              aria-describedby={nameError === null ? undefined : `${nameId}-error`} onChange={event => set(index, {...row, name: event.target.value})}/>
          </div>
          <button id={`authoring-game-name-${index}-remove`} type="button" className="danger-text" aria-label={r.removeName(index + 1)} onClick={() => {
            focus.current = rows.length > 1 ? `authoring-game-name-${Math.min(index, rows.length - 2)}-key` : 'authoring-game-name-add';
            onChange(rows.filter((_, at) => at !== index));
          }}>{r.removeRow}</button>
        </div>
        {keyError && <p id={`${keyId}-error`} className="field-error">{keyError}</p>}
        {nameError && <p id={`${nameId}-error`} className="field-error">{nameError}</p>}
      </div>;
    })}</div>}
    {groupError && <p id="authoring-distribution-names-error" className="field-error">{groupError}</p>}
    <button id="authoring-game-name-add" type="button" disabled={rows.length >= DISTRIBUTION_ENTRIES} onClick={() => {
      focus.current = `authoring-game-name-${rows.length}-key`;
      onChange([...rows, {key: '', name: ''}]);
    }}>{r.addName}</button>
  </fieldset>;
}

// Search aliases, abbreviations and executable names: text rows only, never launch selectors or machine paths.
function Aliases({aliases, focus, onChange}: {aliases: string[]; focus: Focus; onChange: (aliases: string[]) => void}) {
  const r = messages[useLocale()].ui.releaseMetadata;
  const groupError = aliases.length > DISTRIBUTION_ENTRIES ? r.aliasesLimit(DISTRIBUTION_ENTRIES) : null;
  return <fieldset id="authoring-distribution-aliases" className="metadata-group distribution-list"
    aria-describedby={groupError ? 'authoring-distribution-aliases-error' : undefined}>
    <legend>{r.aliases} <HelpTrigger id="authoring-aliases-help" title={r.aliases} hint={r.aliasesHint}><p>{r.aliasesHelp}</p></HelpTrigger></legend>
    {aliases.length === 0 && <p className="muted">{r.aliasesEmpty}</p>}
    {aliases.map((alias, index) => {
      const id = `authoring-alias-${index}`;
      const issue = aliasIssue(alias);
      return <div key={index} className="distribution-alias">
        <div className="enum-item">
          <input id={id} type="text" value={alias} aria-label={r.alias(index + 1)} spellCheck={false} aria-invalid={issue !== null}
            aria-describedby={issue === null ? undefined : `${id}-error`} onChange={event => onChange(aliases.map((item, at) => at === index ? event.target.value : item))}/>
          <button id={`${id}-remove`} type="button" className="danger-text" aria-label={r.removeAlias(index + 1)} onClick={() => {
            focus.current = aliases.length > 1 ? `authoring-alias-${Math.min(index, aliases.length - 2)}` : 'authoring-alias-add';
            onChange(aliases.filter((_, at) => at !== index));
          }}>{r.removeRow}</button>
        </div>
        {issue && <p id={`${id}-error`} className="field-error">{r.aliasIssue[issue]}</p>}
      </div>;
    })}
    {groupError && <p id="authoring-distribution-aliases-error" className="field-error">{groupError}</p>}
    <button id="authoring-alias-add" type="button" disabled={aliases.length >= DISTRIBUTION_ENTRIES} onClick={() => {
      focus.current = `authoring-alias-${aliases.length}`;
      onChange([...aliases, '']);
    }}>{r.addAlias}</button>
  </fieldset>;
}
