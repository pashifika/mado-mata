import {useMemo} from 'react';
import Select from './Select.tsx';
import {JsonErrorNotice, NormalizationNotice} from './MetadataControls.tsx';
import type {FileDraft} from '../authoring.ts';
import {AUTHORING_RUNTIMES, ENTRY_NAMES, HELPER_DEPENDENCY, HELPER_VERSION, bundleIdValid, entryFunctionValid, formatJson, isObject, parseJson, readManifest,
  windowTitleValid, withEntry, withHelper, withTarget} from '../metadata.ts';
import type {JsonObject, ManifestTarget} from '../metadata.ts';
import {portableComponent} from '../state.ts';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';

interface Props {
  draft: FileDraft & {text: string}; disabled: boolean;
  onReplace: (text: string) => void; onOpen: (path: string) => void;
}

function TextField({id, label, value, help, error, onChange}: {id: string; label: string; value: string; help?: string; error: string | null; onChange: (value: string) => void}) {
  const described = [help ? `${id}-help` : '', error ? `${id}-error` : ''].filter(Boolean).join(' ');
  return <div className="field"><label htmlFor={id}>{label}</label>
    <input id={id} type="text" value={value} spellCheck={false} autoCapitalize="off" autoCorrect="off" aria-invalid={error !== null}
      aria-describedby={described || undefined} onChange={event => onChange(event.target.value)}/>
    {help && <p id={`${id}-help`} className="field-help">{help}</p>}
    {error && <p id={`${id}-error`} className="field-error">{error}</p>}</div>;
}

// The manifest as a form: runtime, entry exports, the approved helper and the portable target declaration are edited
// in place; identity and file declarations are facts here, changed through Duplicate and the file menus. The host
// validates every Save, so a rejected edit stays a draft with the refusal shown.
export default function ManifestEditor({draft, disabled, onReplace, onOpen}: Props) {
  const a = messages[useLocale()].ui.authoring;
  const document = useMemo(() => parseJson(draft.text), [draft.text]);
  if (!document.ok) return <JsonErrorNotice id="authoring-manifest-invalid" error={document.error}><p>{a.manifestRestore}</p></JsonErrorNotice>;
  const value = document.value;
  const manifest = readManifest(value);
  if (!isObject(value) || manifest === null) return <p id="authoring-manifest-invalid" className="inline-warning" role="alert">{a.manifestNotObject} {a.manifestRestore}</p>;
  const update = (next: JsonObject) => onReplace(formatJson(draft.base, next));
  const setTarget = (next: ManifestTarget | null) => update(withTarget(value, next));
  const modules = manifest.sources.filter(path => !path.endsWith('.d.ts'));
  const target = manifest.target;
  const open = (path: string) => <button type="button" className="link mono" data-path={path} onClick={() => onOpen(path)}>{path}</button>;

  return <div id="authoring-manifest" className="metadata-view">
    <NormalizationNotice document={document}/>
    <dl className="run-identity metadata-facts">
      <dt>{a.manifestPackageId}</dt><dd><span id="authoring-manifest-package-id" className="mono">{manifest.packageId}</span> <span className="muted">{a.manifestPackageIdHelp}</span></dd>
      <dt>{a.manifestVersion}</dt><dd className="mono">{JSON.stringify(manifest.version ?? null)}</dd>
      <dt>{a.manifestSdk}</dt><dd className="mono">{manifest.sdk}</dd>
      <dt>{a.manifestEntryContract}</dt><dd className="mono">{manifest.entryContract}</dd>
    </dl>
    <fieldset className="metadata-form" disabled={disabled}>
      <div className="field"><label htmlFor="authoring-manifest-runtime">{a.runtime}</label>
        <Select id="authoring-manifest-runtime" value={manifest.runtime} onChange={runtime => update({...value, runtime})}
          options={[...AUTHORING_RUNTIMES.map(runtime => ({value: runtime, label: a.runtimeLabel(runtime)})),
            ...(AUTHORING_RUNTIMES.includes(manifest.runtime) ? [] : [{value: manifest.runtime, label: a.runtimeUnsupported(manifest.runtime), disabled: true}])]}/>
        <p className="field-help">{a.runtimeHelp}</p></div>
      <fieldset id="authoring-manifest-entries" className="metadata-group"><legend>{a.entriesHeading}</legend>
        <p className="field-help">{a.entriesHelp}</p>
        {ENTRY_NAMES.map(name => {
          const entry = manifest.entries[name];
          const moduleId = `authoring-entry-${name}-module`;
          return <div className="two-col" key={name}>
            <div className="field"><label htmlFor={moduleId}>{a.entryModule(a.entryName(name))}</label>
              <Select id={moduleId} value={entry.module} onChange={module => update(withEntry(value, name, 'module', module))}
                options={[...modules.map(path => ({value: path, label: path})),
                  ...(modules.includes(entry.module) ? [] : [{value: entry.module, label: a.moduleUndeclared(entry.module), disabled: true}])]}/></div>
            <TextField id={`authoring-entry-${name}-function`} label={a.entryFunction(a.entryName(name))} value={entry.function}
              error={entryFunctionValid(entry.function) ? null : a.entryFunctionInvalid} onChange={text => update(withEntry(value, name, 'function', text))}/>
          </div>;
        })}
      </fieldset>
      <div className="field">
        <label className="checkbox-label"><input id="authoring-manifest-helper" type="checkbox" checked={manifest.helper}
          onChange={event => update(withHelper(value, event.target.checked))}/>{a.helper(HELPER_DEPENDENCY, HELPER_VERSION)}</label>
        <p className="field-help">{a.helperHelp}</p></div>
      <fieldset id="authoring-manifest-target" className="metadata-group"><legend>{a.targetHeading}</legend>
        <p className="field-help">{a.targetHelp}</p>
        <div className="field"><label className="checkbox-label"><input id="authoring-target-declared" type="checkbox" checked={target !== null}
          onChange={event => setTarget(event.target.checked ? {id: '', windowTitle: null, bundleId: null} : null)}/>{a.targetDeclared}</label></div>
        {target && <>
          <TextField id="authoring-target-id" label={a.targetId} value={target.id} error={portableComponent(target.id) ? null : a.targetIdInvalid}
            onChange={id => setTarget({...target, id})}/>
          <TextField id="authoring-target-title" label={a.targetTitle} value={target.windowTitle ?? ''} help={a.targetTitleHelp}
            error={target.windowTitle === null || windowTitleValid(target.windowTitle) ? null : a.targetTitleInvalid}
            onChange={title => setTarget({...target, windowTitle: title === '' ? null : title})}/>
          <TextField id="authoring-target-bundle" label={a.targetBundle} value={target.bundleId ?? ''} help={a.targetBundleHelp}
            error={target.bundleId === null || bundleIdValid(target.bundleId) ? null : a.targetBundleInvalid}
            onChange={bundle => setTarget({...target, bundleId: bundle === '' ? null : bundle})}/>
        </>}
      </fieldset>
    </fieldset>
    <section id="authoring-manifest-declarations" aria-labelledby="authoring-manifest-declarations-heading">
      <h3 id="authoring-manifest-declarations-heading">{a.declarationsHeading}</h3>
      <p className="field-help">{a.declarationsHelp}</p>
      <dl className="run-identity metadata-facts">
        <dt>{a.declSources}</dt><dd>{manifest.sources.length === 0 ? a.none : <ul className="fact-list">{manifest.sources.map(path => <li key={path}>{open(path)}</li>)}</ul>}</dd>
        <dt>{a.declSchema}</dt><dd>{manifest.schema ? open(manifest.schema) : a.none}</dd>
        <dt>{a.declPresets}</dt><dd>{manifest.profiles.length === 0 ? a.none : <ul className="fact-list">{manifest.profiles.map(([id, path]) =>
          <li key={id}><span className="mono">{id}</span> · {open(path)}</li>)}</ul>}</dd>
        <dt>{a.declAssets}</dt><dd>{manifest.assets.length === 0 ? a.none : <ul className="fact-list">{manifest.assets.map(asset =>
          <li key={asset.id}><span className="mono">{asset.id}</span> · {open(asset.path)} · <span className="mono">{asset.format}</span> · {a.assetDimensions(JSON.stringify(asset.width ?? null), JSON.stringify(asset.height ?? null))}</li>)}</ul>}</dd>
        <dt>{a.declSourceMaps}</dt><dd>{manifest.sourceMaps.length === 0 ? a.none : <ul className="fact-list">{manifest.sourceMaps.map(([module, path]) =>
          <li key={module}><span className="mono">{module}</span> · {open(path)}</li>)}</ul>}</dd>
      </dl>
    </section>
  </div>;
}
