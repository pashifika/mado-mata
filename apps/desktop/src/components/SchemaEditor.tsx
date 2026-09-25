import {useMemo, useState} from 'react';
import Select from './Select.tsx';
import {JsonErrorNotice, NormalizationNotice, NumberInput, RebuildControl, ValuesEditor} from './MetadataControls.tsx';
import type {FileDraft, TypedText} from '../authoring.ts';
import {SCHEMA_BOUNDS, SCHEMA_DEPTH, SCHEMA_TYPES, emptySchema, formatJson, isObject, nodeIssues, parseJson, renameProperty, repairNode, repairable, requiredNames,
  schemaIssues, schemaProperties, schemaType, topDefaults, withKeyword, withProperty, withRequired, withTopDefaults, withType, withoutProperty} from '../metadata.ts';
import type {JsonObject, SchemaType} from '../metadata.ts';
import {optionPath} from '../state.ts';
import type {Json, Schema} from '../types.ts';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';

// `typed` is passed only by the defaults form; other edits keep the draft's recorded provenance.
interface Props {draft: FileDraft & {text: string}; disabled: boolean; onReplace: (text: string, typed?: TypedText[]) => void}

// Stable control IDs follow SchemaForm's convention: a prefix and the encoded value path.
const controlId = (path: string) => `authoring-schema-${encodeURIComponent(path)}`;

function useCopy() {
  const t = messages[useLocale()].ui;
  return {a: t.authoring, s: t.schema};
}

// The node's own problems with their deliberate repairs; a type problem is repaired by choosing a type, and a root
// that is not an object is replaced only through the explicit rebuild.
function IssueRows({node, root, path, onChange}: {node: Json | undefined; root?: boolean; path: string; onChange: (next: Json) => void}) {
  const {a} = useCopy();
  const issues = nodeIssues(node, root);
  if (issues.length === 0) return null;
  return <ul className="schema-issues">{issues.map((issue, index) => <li key={index} className="inline-warning" data-issue={issue.code}>
    {a.schemaIssue(issue)}
    {repairable(issue) && !(root && issue.code === 'node') && <button type="button" data-repair={issue.code} aria-label={`${a.schemaRepair(issue)} · ${path}`}
      onClick={() => onChange(repairNode(node, issue))}>{a.schemaRepair(issue)}</button>}
  </li>)}</ul>;
}

// Field names are committed on Enter or when focus leaves; an empty or taken name is refused and reverted.
function FieldName({id, name, names, onRename}: {id: string; name: string; names: string[]; onRename: (name: string) => void}) {
  const {a} = useCopy();
  const [text, setText] = useState(name);
  const error = text === '' ? a.fieldNameEmpty : text !== name && names.includes(text) ? a.fieldNameTaken : null;
  const commit = () => {
    if (error === null && text !== name) onRename(text); else setText(name);
  };
  return <div className="field schema-name"><label htmlFor={id}>{a.fieldName}</label>
    <input id={id} type="text" value={text} spellCheck={false} autoCapitalize="off" autoCorrect="off" aria-invalid={error !== null}
      aria-describedby={error ? `${id}-error` : undefined} onChange={event => setText(event.target.value)} onBlur={commit}
      onKeyDown={event => {
        if (event.nativeEvent.isComposing) return;
        if (event.key === 'Enter') {
          event.preventDefault();
          commit();
        } else if (event.key === 'Escape') {
          setText(name);
        }
      }}/>
    {error && <p id={`${id}-error`} className="field-error">{error}</p>}</div>;
}

function AddField({path, names, onAdd}: {path: string; names: string[]; onAdd: (name: string, type: SchemaType) => void}) {
  const {a, s} = useCopy();
  const [name, setName] = useState('');
  const [type, setType] = useState<SchemaType>('string');
  const id = `${controlId(path)}-new`;
  const taken = names.includes(name);
  return <div className="schema-add" role="group" aria-label={a.addFieldHeading(path)}>
    <div className="field"><label htmlFor={`${id}-name`}>{a.newFieldName}</label>
      <input id={`${id}-name`} type="text" value={name} spellCheck={false} autoCapitalize="off" autoCorrect="off" aria-invalid={taken}
        aria-describedby={taken ? `${id}-error` : undefined} onChange={event => setName(event.target.value)}/>
      {taken && <p id={`${id}-error`} className="field-error">{a.fieldNameTaken}</p>}</div>
    <div className="field"><label htmlFor={`${id}-type`}>{a.fieldType}</label>
      <Select id={`${id}-type`} value={type} onChange={value => setType(schemaType({type: value}) ?? type)}
        options={SCHEMA_TYPES.map(item => ({value: item, label: a.schemaType(item)}))}/></div>
    <button id={`${id}-add`} type="button" disabled={name === '' || taken} onClick={() => {onAdd(name, type); setName('');}}>{s.addField}</button>
  </div>;
}

function Properties({node, path, depth, onChange}: {node: JsonObject; path: string; depth: number; onChange: (next: JsonObject) => void}) {
  const {a} = useCopy();
  const properties = schemaProperties(node);
  const required = requiredNames(node);
  const names = properties.map(([name]) => name);
  return <div className="schema-properties" data-schema-fields={path}>
    {properties.length === 0 && <p className="muted">{a.schemaNoFields}</p>}
    {properties.map(([name, child]) => <SchemaNode key={name} node={child} path={optionPath(path, name)} depth={depth + 1}
      onChange={next => onChange(withProperty(node, name, next))}
      field={{name, names, required: required.includes(name), top: depth === 0,
        onRename: to => onChange(renameProperty(node, name, to)),
        onRequired: on => onChange(withRequired(node, name, on)),
        onRemove: () => onChange(withoutProperty(node, name))}}/>)}
    {depth < SCHEMA_DEPTH && <AddField path={path} names={names} onAdd={(name, type) => onChange(withProperty(node, name, withType(undefined, type)))}/>}
  </div>;
}

function EnumEditor({node, id, onChange}: {node: JsonObject; id: string; onChange: (next: Json) => void}) {
  const {a, s} = useCopy();
  // A non-list enum is reported with its repair instead of being edited as a list.
  const listed = node.enum;
  if (listed !== undefined && !Array.isArray(listed)) return null;
  const values = listed ?? [];
  const set = (next: Json[]) => onChange(withKeyword(node, 'enum', next.length === 0 ? undefined : next));
  return <fieldset id={`${id}-enum`} className="metadata-group schema-enum"><legend>{a.enumHeading}</legend>
    <p className="field-help">{a.enumHelp}</p>
    {values.map((item, index) => <div className="enum-item" key={index}>
      <input id={`${id}-enum-${index}`} type="text" aria-label={a.enumValue(index + 1)} value={typeof item === 'string' ? item : JSON.stringify(item)} spellCheck={false}
        onChange={event => set(values.map((old, at) => at === index ? event.target.value : old))}/>
      <button id={`${id}-enum-${index}-remove`} type="button" aria-label={a.enumRemove(index + 1)} onClick={() => set(values.filter((_, at) => at !== index))}>{s.remove}</button>
    </div>)}
    <button id={`${id}-enum-add`} type="button" onClick={() => set([...values, ''])}>{a.enumAdd}</button>
  </fieldset>;
}

interface FieldControls {
  name: string; names: string[]; required: boolean;
  // Top-level fields: their defaults are edited in the defaults form, where they apply.
  top: boolean;
  onRename: (name: string) => void; onRequired: (required: boolean) => void; onRemove: () => void;
}

// One schema node: an object member (with name, required and remove controls) or an array's item description.
function SchemaNode({node, path, depth, onChange, field}: {node: Json; path: string; depth: number; onChange: (next: Json) => void; field?: FieldControls}) {
  const {a, s} = useCopy();
  const id = controlId(path);
  const type = schemaType(node);
  const object = isObject(node) ? node : null;
  const bounds = type === null ? undefined : SCHEMA_BOUNDS[type];
  const issues = nodeIssues(node);
  return <section className="schema-node" data-schema-path={path} aria-label={path}>
    <div className="schema-node-heading">
      {field ? <FieldName id={`${id}-name`} name={field.name} names={field.names} onRename={field.onRename}/> : <span className="schema-node-label">{a.items}</span>}
      <div className="field schema-type"><label htmlFor={`${id}-type`}>{a.fieldType}</label>
        <Select id={`${id}-type`} value={type ?? ''} onChange={value => {
          const next = schemaType({type: value});
          if (next) onChange(withType(node, next));
        }} options={[...(type === null ? [{value: '', label: a.unsupportedType(JSON.stringify(object?.type ?? null)), disabled: true}] : []),
          ...SCHEMA_TYPES.map(item => ({value: item, label: a.schemaType(item)}))]}/></div>
      {field && <label className="checkbox-label"><input id={`${id}-required`} type="checkbox" checked={field.required}
        onChange={event => field.onRequired(event.target.checked)}/>{a.fieldRequired}</label>}
      {field && <button id={`${id}-remove`} type="button" className="danger-text" onClick={field.onRemove}>{s.removeField}</button>}
    </div>
    <IssueRows node={node} path={path} onChange={onChange}/>
    {depth > SCHEMA_DEPTH && <p className="inline-warning">{a.schemaIssue({code: 'depth'})}</p>}
    {object && bounds && <div className="two-col">{[bounds[0], bounds[1]].map(key => <NumberInput key={key} id={`${id}-${key}`} label={a.boundLabel(key)}
      value={object[key]} whole={bounds[2]} invalid={issues.some(issue => (issue.code === 'bound' && issue.key === key) || issue.code === 'reversed')}
      onChange={next => onChange(withKeyword(object, key, next))}/>)}</div>}
    {object && type === 'string' && <EnumEditor node={object} id={id} onChange={onChange}/>}
    {object && type === 'array' && object.items !== undefined && <div className="schema-items">
      <SchemaNode node={object.items} path={`${path}[]`} depth={depth + 1} onChange={items => onChange({...object, items})}/></div>}
    {object && type === 'object' && isObject(object.properties) && <Properties node={object} path={path} depth={depth} onChange={onChange}/>}
    {object && object.default !== undefined && !field?.top && <p className="muted schema-default">{a.nestedDefault(JSON.stringify(object.default))}
      <button id={`${id}-remove-default`} type="button" className="text-button" onClick={() => onChange(withKeyword(object, 'default', undefined))}>{a.removeDefault}</button></p>}
  </section>;
}

// The options schema as a form in the host's keyword set. Unsupported members and malformed parts are kept and
// reported until a deliberate repair; text that is not JSON is kept as saved until an explicit rebuild.
export default function SchemaEditor({draft, disabled, onReplace}: Props) {
  const {a} = useCopy();
  const document = useMemo(() => parseJson(draft.text), [draft.text]);
  const update = (next: Json, typed?: TypedText[]) => onReplace(formatJson(draft.base, next), typed);
  const rebuild = <RebuildControl id="authoring-schema-rebuild" label={a.schemaRebuild} question={a.schemaRebuildConfirm} disabled={disabled}
    onConfirm={() => update(emptySchema())}/>;
  if (!document.ok) return <JsonErrorNotice id="authoring-schema-invalid" error={document.error}><div className="button-row">{rebuild}</div></JsonErrorNotice>;
  const root = document.value;
  if (!isObject(root) || schemaType(root) !== 'object') {
    return <section id="authoring-schema-invalid" className="metadata-invalid" role="alert">
      <fieldset className="metadata-form" disabled={disabled}><IssueRows node={root} root path="$" onChange={update}/></fieldset>
      <div className="button-row">{rebuild}</div></section>;
  }
  const problems = schemaIssues(root);
  return <div id="authoring-schema" className="metadata-view">
    <NormalizationNotice document={document}/>
    {problems.length > 0 && <section id="authoring-schema-problems" className="inline-warning" role="status">
      <strong>{a.schemaProblems(problems.length)}</strong> <span>{a.schemaProblemsHelp}</span>
      <ul>{problems.map((item, index) => <li key={index}><code>{item.path}</code> {a.schemaIssue(item.issue)}</li>)}</ul></section>}
    <fieldset className="metadata-form" disabled={disabled}>
      <IssueRows node={root} root path="$" onChange={update}/>
      <h3>{a.schemaFields}</h3>
      <p className="field-help">{a.schemaFieldsHelp}</p>
      {isObject(root.properties) && <Properties node={root} path="$" depth={0} onChange={update}/>}
      <h3 id="authoring-schema-defaults-heading">{a.defaultsHeading}</h3>
      <p className="field-help">{a.defaultsHelp}</p>
      {problems.length === 0
        ? <div id="authoring-schema-defaults" role="group" aria-labelledby="authoring-schema-defaults-heading">
          <ValuesEditor schema={root as unknown as Schema} value={topDefaults(root)} typed={draft.typed} idPrefix="authoring-default"
            onChange={(defaults, typed) => update(withTopDefaults(root, defaults), typed)}/></div>
        : <p className="muted">{a.defaultsBlocked}</p>}
      <div className="button-row">{rebuild}</div>
    </fieldset>
  </div>;
}
