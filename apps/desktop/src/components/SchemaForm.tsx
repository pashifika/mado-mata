import Select from './Select.tsx';
import type {Json, Schema} from '../types.ts';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';
import {optionPath} from '../state.ts';

interface FormProps {
  schema: Schema;
  value: Record<string, Json>;
  onChange: (value: Record<string, Json>) => void;
  errors: Record<string, string>;
  // Distinguishes field IDs when two editors of the same schema are on one page; the default is the ordinary editor.
  idPrefix?: string;
}

function object(value: Json): value is Record<string, Json> {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function emptyValue(schema: Schema): Json {
  if (schema.type === 'object') return {};
  if (schema.type === 'array') return [];
  if (schema.type === 'boolean') return false;
  return '';
}

interface FieldProps {
  schema: Schema;
  value: Json;
  path: string;
  onChange: (value: Json) => void;
  errors: Record<string, string>;
  idPrefix: string;
}

function Field({schema, value, path, onChange, errors, idPrefix}: FieldProps) {
  const locale = useLocale();
  const t = messages[locale].ui;
  const id = `${idPrefix}-${encodeURIComponent(path)}`;
  if (schema.type === 'object' && object(value)) {
    return <div id={id} className="object-fields" role="group" aria-label={path}>
      {Object.entries(schema.properties ?? {}).map(([name, child]) => {
        const present = Object.hasOwn(value, name);
        const childPath = optionPath(path, name);
        const topDefault = path === '$' && child.default !== undefined;
        return <section className="schema-field" key={name}>
          <div className="field-heading">
            <label htmlFor={`${idPrefix}-${encodeURIComponent(childPath)}`}>{name}</label>
            <span className="field-type">{child.type} · {schema.required?.includes(name) ? t.schema.required : t.schema.optional}</span>
            <button className="text-button" type="button" aria-label={t.schema.fieldAction(present, childPath)} onClick={() => {
              const next = {...value};
              if (present) delete next[name];
              else next[name] = emptyValue(child);
              onChange(next);
            }}>{present ? t.schema.omit : t.schema.addField}</button>
          </div>
          {present ? <Field schema={child} value={value[name]} path={childPath} errors={errors} idPrefix={idPrefix}
            onChange={next => onChange({...value, [name]: next})}/>
            : <p className="muted omitted">{topDefault ? t.schema.omittedDefault(JSON.stringify(child.default)) : t.schema.omitted}</p>}
        </section>;
      })}
      {Object.keys(value).filter(name => !Object.hasOwn(schema.properties ?? {}, name)).map(name => <div className="inline-warning" key={name}>
        {t.schema.unknown(optionPath(path, name), JSON.stringify(value[name]))}
        <button type="button" onClick={() => {const next = {...value}; delete next[name]; onChange(next);}}>{t.schema.removeField}</button>
      </div>)}
    </div>;
  }
  if (schema.type === 'array' && Array.isArray(value) && schema.items) {
    function move(index: number, offset: number) {
      const next = [...value as Json[]];
      [next[index], next[index + offset]] = [next[index + offset], next[index]];
      onChange(next);
    }
    return <div id={id} className="array-field" role="group" aria-label={path}>
      <p className="muted">{t.schema.ordered(value.length, schema.minItems, schema.maxItems)}</p>
      {value.map((item, index) => <div className="array-item" key={index}>
        <span className="item-index">{index + 1}</span>
        <div className="array-value"><Field schema={schema.items!} value={item} path={`${path}[${index}]`} errors={errors} idPrefix={idPrefix}
          onChange={next => onChange(value.map((old, at) => at === index ? next : old))}/></div>
        <div className="array-actions">
          <button type="button" aria-label={t.schema.moveUp(`${path}[${index}]`)} disabled={index === 0} onClick={() => move(index, -1)}>{t.schema.up}</button>
          <button type="button" aria-label={t.schema.moveDown(`${path}[${index}]`)} disabled={index === value.length - 1} onClick={() => move(index, 1)}>{t.schema.down}</button>
          <button type="button" aria-label={t.schema.removeItem(`${path}[${index}]`)} onClick={() => onChange(value.filter((_, at) => at !== index))}>{t.schema.remove}</button>
        </div>
      </div>)}
      <button type="button" disabled={schema.maxItems !== undefined && value.length >= schema.maxItems}
        onClick={() => onChange([...value, emptyValue(schema.items!)])}>{t.schema.addItem}</button>
    </div>;
  }
  if (schema.enum && (typeof value === 'string' || typeof value === 'number' || typeof value === 'boolean')) {
    const index = schema.enum.findIndex(item => item === value);
    return <Select id={id} aria-label={path} value={index < 0 ? '' : String(index)}
      onChange={selected => {if (selected !== '') onChange(schema.enum![Number(selected)]);}}
      options={[{value: '', label: index < 0 && value !== '' ? t.schema.invalid(String(value)) : t.schema.choose, disabled: true},
        ...schema.enum.map((item, at) => ({value: String(at), label: String(item)}))]}/>;
  }
  if (schema.type === 'boolean' && typeof value === 'boolean') {
    return <label className="checkbox-label"><input id={id} aria-label={path} type="checkbox" checked={value}
      onChange={event => onChange(event.target.checked)}/>{value ? t.schema.enabled : t.schema.disabled}</label>;
  }
  if (schema.type === 'string' && typeof value === 'string') {
    return <div><input id={id} aria-label={path} type="text" value={value} onChange={event => onChange(event.target.value)}/>
      {(schema.minLength !== undefined || schema.maxLength !== undefined) && <small className="muted">{t.schema.length(schema.minLength ?? 0, schema.maxLength)}</small>}</div>;
  }
  if ((schema.type === 'integer' || schema.type === 'number') && (typeof value === 'number' || typeof value === 'string')) {
    return <div><input id={id} aria-label={path} type="text" inputMode="decimal" value={String(value)}
      aria-invalid={Boolean(errors[path])} aria-describedby={`${id}-hint`}
      onChange={event => onChange(event.target.value)}/>
      <small id={`${id}-hint`} className={errors[path] ? 'field-error' : 'muted'}>{errors[path] ?? t.schema.numeric(schema.type, schema.minimum, schema.maximum)}</small></div>;
  }
  return <div id={id} className="inline-warning">{t.schema.mismatch(schema.type, JSON.stringify(value))}
    <button type="button" onClick={() => onChange(emptyValue(schema))}>{t.schema.replace(schema.type)}</button>
  </div>;
}

export default function SchemaForm({schema, value, onChange, errors, idPrefix = 'option'}: FormProps) {
  return <Field schema={schema} value={value} path="$" errors={errors} idPrefix={idPrefix} onChange={next => onChange(next as Record<string, Json>)}/>;
}
