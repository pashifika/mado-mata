import {useMemo} from 'react';
import {JsonErrorNotice, NormalizationNotice, RebuildControl, ValuesEditor} from './MetadataControls.tsx';
import type {FileDraft, TypedText} from '../authoring.ts';
import {formatJson, isObject, parseJson, presetIssues, repairPreset, schemaIssues, starterPreset} from '../metadata.ts';
import type {Json, Schema} from '../types.ts';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';

interface Props {
  draft: FileDraft & {text: string}; presetId: string | null; packageId: string;
  // The options schema draft, including unsaved edits, so values follow the schema being designed.
  schema: FileDraft | undefined;
  // `typed` is passed only by the options form; repairs and rebuilds keep the draft's recorded provenance.
  disabled: boolean; onReplace: (text: string, typed?: TypedText[]) => void; onOpen: (path: string) => void;
}

// A packaged preset (a file in this package) edited through the options schema's form. It is never a saved
// profile: App storage is not read or written here.
export default function PresetEditor({draft, presetId, packageId, schema, disabled, onReplace, onOpen}: Props) {
  const a = messages[useLocale()].ui.authoring;
  const document = useMemo(() => parseJson(draft.text), [draft.text]);
  const schemaText = schema?.text ?? null;
  const schemaDocument = useMemo(() => schemaText === null ? null : parseJson(schemaText), [schemaText]);
  const schemaRoot = schemaDocument?.ok ? schemaDocument.value : undefined;
  const schemaVersion = isObject(schemaRoot) ? schemaRoot.version : undefined;
  const formSchema = schemaRoot !== undefined && schemaIssues(schemaRoot).length === 0 ? schemaRoot as unknown as Schema : null;
  const update = (next: Json, typed?: TypedText[]) => onReplace(formatJson(draft.base, next), typed);
  const rebuild = <RebuildControl id="authoring-preset-rebuild" label={a.presetRebuild} question={a.presetRebuildConfirm} disabled={disabled}
    onConfirm={() => update(starterPreset(packageId, schemaVersion))}/>;
  const note = <p className="authority-note">{a.presetNote}</p>;
  if (!document.ok) {
    return <div id="authoring-preset" className="metadata-view">{note}
      <JsonErrorNotice id="authoring-preset-invalid" error={document.error}><div className="button-row">{rebuild}</div></JsonErrorNotice></div>;
  }
  const value = document.value;
  if (!isObject(value)) {
    return <div id="authoring-preset" className="metadata-view">{note}
      <section id="authoring-preset-invalid" className="inline-warning" role="alert">{a.presetIssue({code: 'root'})}<div className="button-row">{rebuild}</div></section></div>;
  }
  const issues = presetIssues(value, packageId, schemaVersion);
  const options = value.options;
  return <div id="authoring-preset" className="metadata-view">
    {note}
    <NormalizationNotice document={document}/>
    <dl className="run-identity metadata-facts">
      <dt>{a.presetId}</dt><dd className="mono">{presetId ?? a.none}</dd>
      <dt>{a.presetPackage}</dt><dd className="mono">{typeof value.package_id === 'string' ? value.package_id : JSON.stringify(value.package_id ?? null)}</dd>
      <dt>{a.presetSchemaVersion}</dt><dd className="mono">{JSON.stringify(value.schema_version ?? null)}</dd>
    </dl>
    <fieldset className="metadata-form" disabled={disabled}>
      {issues.length > 0 && <ul id="authoring-preset-issues" className="schema-issues">{issues.map((issue, index) => <li key={index} className="inline-warning" data-issue={issue.code}>
        {a.presetIssue(issue)}
        <button type="button" data-repair={issue.code} onClick={() => update(repairPreset(value, issue, packageId, schemaVersion))}>{a.presetRepair(issue)}</button>
      </li>)}</ul>}
      <h3 id="authoring-preset-options-heading">{a.presetOptions}</h3>
      <p className="field-help">{a.presetOptionsHelp}</p>
      {formSchema === null
        ? <p className="inline-warning">{a.presetSchemaBlocked} {schema && <button type="button" className="link" data-path={schema.path} onClick={() => onOpen(schema.path)}>{a.openSchema}</button>}</p>
        : isObject(options) && <div id="authoring-preset-options" role="group" aria-labelledby="authoring-preset-options-heading">
          <ValuesEditor schema={formSchema} value={options} typed={draft.typed} idPrefix="authoring-preset"
            onChange={(next, typed) => update({...value, options: next}, typed)}/></div>}
      <div className="button-row">{rebuild}</div>
    </fieldset>
  </div>;
}
