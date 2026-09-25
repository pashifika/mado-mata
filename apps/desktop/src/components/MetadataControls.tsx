import {useState} from 'react';
import type {ReactNode} from 'react';
import SchemaForm from './SchemaForm.tsx';
import {constraintValue, currentValues, editValues, jsonEqual, loadValues, typedText} from '../metadata.ts';
import type {TypedText} from '../authoring.ts';
import type {JsonDocument, JsonError} from '../metadata.ts';
import {readDraft} from '../state.ts';
import type {Json, Schema} from '../types.ts';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';

// A document that is not JSON is described, never rewritten: its text stays as saved until a deliberate rebuild.
export function JsonErrorNotice({id, error, children}: {id: string; error: JsonError; children?: ReactNode}) {
  const a = messages[useLocale()].ui.authoring;
  return <section id={id} className="inline-warning metadata-invalid" role="alert">
    <strong>{a.jsonInvalid}</strong> <span>{a.jsonLocation(error.problem, error.line, error.column)}</span>
    <p>{a.jsonKept}</p>
    {children}
  </section>;
}

// Discloses, before the first edit, what rewriting the document would normalize.
export function NormalizationNotice({document}: {document: JsonDocument}) {
  const a = messages[useLocale()].ui.authoring;
  if (!document.ok || (document.numbers.length === 0 && document.duplicates.length === 0)) return null;
  return <div id="authoring-normalization" className="inline-warning">
    {document.numbers.length > 0 && <p>{a.normalizedNumbers(document.numbers.join(', '))}</p>}
    {document.duplicates.length > 0 && <p>{a.normalizedDuplicates(document.duplicates.join(', '))}</p>}
  </div>;
}

// Replaces a whole document the form cannot repair member by member; only an explicit confirmation does it.
export function RebuildControl({id, label, question, disabled, onConfirm}: {id: string; label: string; question: string; disabled: boolean; onConfirm: () => void}) {
  const a = messages[useLocale()].ui.authoring;
  const [confirming, setConfirming] = useState(false);
  return confirming
    ? <span className="confirm-row" role="alertdialog" aria-labelledby={`${id}-text`}><span id={`${id}-text`}>{question}</span>
      <button id={`${id}-confirm`} type="button" className="danger-text" disabled={disabled} onClick={() => {setConfirming(false); onConfirm();}}>{a.rebuildConfirmed}</button>
      <button id={`${id}-cancel`} type="button" autoFocus onClick={() => setConfirming(false)}>{a.cancel}</button></span>
    : <button id={id} type="button" disabled={disabled} onClick={() => setConfirming(true)}>{label}</button>;
}

// A constraint keeps exactly the typed text; the document receives what constraintValue makes of it.
export function NumberInput({id, label, value, whole, invalid, onChange}: {
  id: string; label: string; value: Json | undefined; whole: boolean; invalid: boolean; onChange: (value: Json | undefined) => void;
}) {
  const a = messages[useLocale()].ui.authoring;
  const [typed, setTyped] = useState<{text: string; value: Json | undefined} | null>(null);
  const shown = typed !== null && jsonEqual(typed.value, value) ? typed.text
    : value === undefined ? '' : typeof value === 'string' ? value : JSON.stringify(value);
  return <div className="field metadata-number"><label htmlFor={id}>{label}</label>
    <input id={id} type="text" inputMode="decimal" value={shown} spellCheck={false} aria-invalid={invalid} aria-describedby={`${id}-hint`}
      onChange={event => {
        const text = event.target.value;
        const next = constraintValue(text);
        setTyped({text, value: next});
        onChange(next);
      }}/>
    <small id={`${id}-hint`} className={invalid ? 'field-error' : 'muted'}>{whole ? a.countHint : a.numberHint}</small></div>;
}

// The overlay keeps the exact text being typed while the document receives converted values; see ValuesState. The
// provenance of typed numeric text travels with the file draft, so the form can be unmounted and shown again.
export function ValuesEditor({schema, value, typed, idPrefix, onChange}: {
  schema: Schema; value: Record<string, Json>; typed: readonly TypedText[]; idPrefix: string;
  onChange: (value: Record<string, Json>, typed: TypedText[]) => void;
}) {
  const locale = useLocale();
  const [state, setState] = useState(() => loadValues(schema, value, typed));
  const current = currentValues(state, schema, value, typed);
  const stored = new Set(current.stored.map(entry => entry.path));
  const {errors} = readDraft(schema, current.draft, locale, stored);
  return <SchemaForm schema={schema} value={current.draft} errors={errors} idPrefix={idPrefix} stored={stored} onChange={(draft, edit) => {
    const next = editValues(current, draft, edit);
    setState(next);
    onChange(next.committed, typedText(next));
  }}/>;
}
