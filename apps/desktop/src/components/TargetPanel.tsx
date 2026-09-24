import {useEffect, useRef, useState} from 'react';
import type {ReactNode} from 'react';
import Select from './Select.tsx';
import {BoundedRecord} from './ResultPanel.tsx';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';
import {record, text} from '../state.ts';
import {currentTargetDraft, readTargetDraft, targetDirty, targetExpectation} from '../target.ts';
import type {TargetDraft, TargetField, TargetState} from '../target.ts';
import type {Json, TargetResolution} from '../types.ts';

export interface TargetHandlers {
  edit:(draft:TargetDraft) => void; reload:() => void; check:() => void;
  save:(reviewedResolution?:TargetResolution) => void; discard:() => void; remove:() => void;
}

export default function TargetPanel({state, handlers, locked, active}: {
  state:TargetState; handlers:TargetHandlers; locked:boolean; active:boolean;
}) {
  const locale = useLocale();
  const t = messages[locale].ui.target;
  const [confirmRemove, setConfirmRemove] = useState(false);
  const removeButton = useRef<HTMLButtonElement>(null);
  const cancelRemove = useRef<HTMLButtonElement>(null);
  const refocusRemove = useRef(false);
  const focusArgument = useRef<number|null>(null);
  useEffect(() => {
    if (confirmRemove) cancelRemove.current?.focus();
    else if (refocusRemove.current) {
      refocusRemove.current = false;
      const button = removeButton.current;
      (button && !button.disabled ? button : document.getElementById('target-heading'))?.focus();
    }
  }, [confirmRemove]);
  useEffect(() => {
    if (focusArgument.current === null) return;
    document.getElementById(`target-argument-${focusArgument.current}`)?.focus();
    focusArgument.current = null;
  }, [state.draft.arguments]);
  const draft = state.draft;
  const parsed = readTargetDraft(draft);
  const expectation = targetExpectation(state);
  const disabled = locked || active || expectation === null || state.operation !== null;
  // A pending metadata command need not freeze typing; its ticket guards any later response.
  const editingDisabled = state.view === null || (locked && state.operation === null);
  const binding = state.view?.record.binding;
  const observation = state.observation;
  const currentObservation = observation && currentTargetDraft(state, observation.ticket);
  const currentIssue = state.issue && currentTargetDraft(state, state.issue.ticket);
  const review = state.review && currentTargetDraft(state, state.review.ticket) ? state.review : null;
  const errorContext = record(state.issue?.fault.context);
  const field = text(errorContext.field);
  const hostFields:Record<string,TargetField> = {
    game:'gamePath', 'game.path':'gamePath', 'game.kind':'gameKind', launcher:'launcherPath', 'launcher.path':'launcherPath',
    'launcher.kind':'launcherKind', working_directory:'workingDirectory', arguments:'arguments', window_title:'windowTitle',
    'input.route':'route', 'input.focus':'focus', 'input.pointer_mode':'pointerMode', 'input.click_hold_ms':'clickHold',
  };
  function fieldError(name:TargetField):string|null {
    if (currentIssue && field && hostFields[field] === name) return state.issue!.fault.message;
    const error = parsed.errors[name];
    return error ? t.fieldErrors[error] : null;
  }
  function fieldBox(name:TargetField, id:string, label:string, control:ReactNode) {
    const error = fieldError(name);
    return <div className="field"><label htmlFor={id}>{label}</label>{control}
      {error && <p id={`${id}-error`} className="field-error">{error}</p>}</div>;
  }
  function attributes(name:TargetField, id:string) {
    return {'aria-invalid':fieldError(name) !== null, 'aria-describedby':fieldError(name) ? `${id}-error` : undefined};
  }
  function input(name:'gamePath'|'launcherPath'|'workingDirectory'|'windowTitle'|'clickHold', id:string, label:string, readOnly = false) {
    return fieldBox(name, id, label, <input id={id} value={draft[name]} spellCheck={false} readOnly={readOnly}
      inputMode={name === 'clickHold' ? 'numeric' : undefined} {...attributes(name, id)}
      onChange={event => handlers.edit({...draft, [name]:event.target.value})}/>);
  }
  const kinds = [{value:'', label:t.choose}, {value:'executable', label:t.executable}, {value:'bundle', label:t.bundle}];
  const status = currentIssue ? t.failed : review ? t.changed : currentObservation ? t.passed : targetDirty(state) ? t.modified
    : binding ? state.view?.compatible ? t.unchecked : t.incompatibleStatus : t.noBinding;
  function moveArgument(index:number, offset:number) {
    const arguments_ = [...draft.arguments];
    [arguments_[index], arguments_[index + offset]] = [arguments_[index + offset], arguments_[index]];
    handlers.edit({...draft, arguments:arguments_});
    document.getElementById(`target-argument-${index + offset}`)?.focus();
  }
  function closeRemoval(remove:boolean) {
    refocusRemove.current = true;
    setConfirmRemove(false);
    if (remove) handlers.remove();
  }
  return <section id="target-panel" className="panel target-panel" aria-labelledby="target-heading">
    <div className="panel-heading"><h2 id="target-heading" tabIndex={-1}>{t.heading}</h2><span className="tag">macOS</span></div>
    <div className="panel-body">
      <p className="field-help">{t.introduction}</p>
      {state.declaration ? <dl className="run-identity">
        <dt>{t.declaration}</dt><dd>{state.declaration.id}</dd>
        <dt>{t.declarationIdentity}</dt><dd><code>{state.context.declaration_identity}</code></dd>
        <dt>{t.packageTitle}</dt><dd>{state.declaration.window_title ?? t.localTitle}</dd>
      </dl> : <p id="target-no-declaration" className="muted">{t.noDeclaration}</p>}
      <div id="target-status" role="status" aria-live="polite">
        <p>{state.operation === 'read' ? t.loading : !state.loaded ? t.unloaded : status}</p>
        {state.persisted && <p>{state.persisted === 'saved' ? t.saved : t.removed}</p>}
        {state.refreshRequired && <p className="inline-warning">{state.readError ? state.persisted ? t.refreshFailed : t.readFailed : t.refreshPending}</p>}
        {state.reconcile && <p className="inline-warning">{t.conflict}</p>}
        {active && <p className="muted">{t.active}</p>}
      </div>
      {state.readError && <section className="fault" role="alert"><strong>{t.readFailed} · {state.readError.category}</strong><p>{state.readError.message}</p>
        <details><summary>{t.privateDiagnostic}</summary><BoundedRecord value={state.readError.context}/></details></section>}
      {state.issue && <section className="fault" role="alert"><strong>{state.issue.fault.category}</strong>
        {!currentIssue && <p>{t.earlier}</p>}<p>{state.issue.fault.message}</p>
        {field && <code>{field}{text(errorContext.stage) ? ` · ${text(errorContext.stage)}` : ''}</code>}
        <details><summary>{t.privateDiagnostic}</summary><BoundedRecord value={state.issue.fault.context}/></details></section>}
      <div className="button-row"><button id="target-reload" type="button" disabled={locked || state.operation !== null} onClick={handlers.reload}>{t.reload}</button>
        {state.view && <span className="muted">{t.revision(state.view.record.revision)}</span>}</div>
      {binding && <>
        {!state.view?.compatible && <p id="target-incompatible" className="inline-warning">{t.incompatible}</p>}
        <details id="target-saved"><summary>{state.view?.compatible ? t.savedResolution : t.retained}</summary>
          <dl className="run-identity"><dt>{t.bindingId}</dt><dd>{binding.id}</dd><dt>{t.declaration}</dt><dd>{binding.target_id}</dd></dl>
          <BoundedRecord value={binding as unknown as Json}/></details>
      </>}
      {state.declaration && <>
        <p className="authority-note">{t.privacy}</p>
        <fieldset className="target-fields" disabled={editingDisabled}>
          <legend>{t.game}</legend>
          <div className="two-col">
            {fieldBox('gameKind', 'target-game-kind', t.gameKind,
              <Select id="target-game-kind" value={draft.gameKind} options={kinds} disabled={editingDisabled} {...attributes('gameKind', 'target-game-kind')}
                onChange={value => handlers.edit({...draft, gameKind:value as TargetDraft['gameKind']})}/>)}
            {input('gamePath', 'target-game-path', t.gamePath)}
          </div>
          <h3>{t.launcher}</h3>
          <label className="checkbox-label"><input id="target-separate-launcher" type="checkbox" checked={draft.separateLauncher}
            onChange={event => handlers.edit({...draft, separateLauncher:event.target.checked})}/>{t.separateLauncher}</label>
          <p className="field-help">{t.launcherHelp}</p>
          {draft.separateLauncher && <div className="two-col">
            {fieldBox('launcherKind', 'target-launcher-kind', t.launcherKind,
              <Select id="target-launcher-kind" value={draft.launcherKind} options={kinds} disabled={editingDisabled} {...attributes('launcherKind', 'target-launcher-kind')}
                onChange={value => handlers.edit({...draft, launcherKind:value as TargetDraft['launcherKind']})}/>)}
            {input('launcherPath', 'target-launcher-path', t.launcherPath)}
          </div>}
          <h3 id="target-arguments-label">{t.arguments}</h3><p id="target-arguments-help" className="field-help">{t.argumentsHelp}</p>
          <ol id="target-arguments" className="target-arguments" aria-labelledby="target-arguments-label" aria-describedby="target-arguments-help">
            {draft.arguments.map((value, index) => <li key={index}>
              <div className="field"><label htmlFor={`target-argument-${index}`}>{t.argument(index + 1)}</label>
                <input id={`target-argument-${index}`} value={value} spellCheck={false} {...attributes('arguments', 'target-arguments')}
                  onChange={event => handlers.edit({...draft, arguments:draft.arguments.map((arg, at) => at === index ? event.target.value : arg)})}/></div>
              <div className="button-row">
                <button type="button" disabled={index === 0} aria-label={`${t.moveUp} · ${t.argument(index + 1)}`} onClick={() => moveArgument(index, -1)}>{t.moveUp}</button>
                <button type="button" disabled={index === draft.arguments.length - 1} aria-label={`${t.moveDown} · ${t.argument(index + 1)}`} onClick={() => moveArgument(index, 1)}>{t.moveDown}</button>
                <button type="button" aria-label={`${t.removeArgument} · ${t.argument(index + 1)}`} onClick={() => {
                  handlers.edit({...draft, arguments:draft.arguments.filter((_, at) => at !== index)});
                  document.getElementById('target-add-argument')?.focus();
                }}>{t.removeArgument}</button>
              </div>
            </li>)}
          </ol>
          {fieldError('arguments') && <p id="target-arguments-error" className="field-error">{fieldError('arguments')}</p>}
          <button id="target-add-argument" type="button" disabled={draft.arguments.length >= 32} onClick={() => {
            focusArgument.current = draft.arguments.length;
            handlers.edit({...draft, arguments:[...draft.arguments, '']});
          }}>{t.addArgument}</button>
          <div className="two-col">{input('workingDirectory', 'target-working-directory', t.workingDirectory)}{input('windowTitle', 'target-window-title', t.windowTitle, state.declaration.window_title !== null)}</div>
          <p className="field-help">{t.windowHelp}</p>
          <h3>{t.policy}</h3><p className="field-help">{t.policyHelp}</p>
          <div className="two-col">
            {fieldBox('route', 'target-route', t.route, <Select id="target-route" value={draft.route} disabled={editingDisabled} {...attributes('route', 'target-route')}
              options={[{value:'', label:t.choose}, {value:'process_directed', label:t.processDirected}, {value:'system', label:t.system}]}
              onChange={value => handlers.edit({...draft, route:value as TargetDraft['route']})}/>)}
            {fieldBox('focus', 'target-focus', t.focus, <Select id="target-focus" value={draft.focus} disabled={editingDisabled} {...attributes('focus', 'target-focus')}
              options={[{value:'', label:t.choose}, {value:'preserve', label:t.preserve}, {value:'require_focused', label:t.requireFocused}]}
              onChange={value => handlers.edit({...draft, focus:value as TargetDraft['focus']})}/>)}
            {draft.route === 'process_directed' && fieldBox('pointerMode', 'target-pointer-mode', t.pointerMode,
              <Select id="target-pointer-mode" value={draft.pointerMode} disabled={editingDisabled} {...attributes('pointerMode', 'target-pointer-mode')}
                options={[{value:'', label:t.choose}, {value:'core_graphics', label:t.coreGraphics}, {value:'appkit_background', label:t.appkitBackground}]}
                onChange={value => handlers.edit({...draft, pointerMode:value as TargetDraft['pointerMode']})}/>)}
            {input('clickHold', 'target-click-hold', t.clickHold)}
          </div>
        </fieldset>
        <div className="button-row">
          <button id="target-save" type="button" className="primary" disabled={disabled || !parsed.configuration || review !== null} onClick={() => handlers.save()}>{binding && !state.view?.compatible ? t.replace : t.save}</button>
          <button id="target-check" type="button" disabled={disabled || !parsed.configuration} onClick={handlers.check}>{t.check}</button>
          <button id="target-discard" type="button" disabled={state.operation !== null || expectation === null || !targetDirty(state)} onClick={handlers.discard}>{t.discard}</button>
        </div>
      </>}
      {review && <section id="target-resolution-review" className="legacy-box" aria-labelledby="target-review-heading">
        <h3 id="target-review-heading">{t.review}</h3><p>{t.reviewHelp}</p>
        <h4>{t.previousResolution}</h4><BoundedRecord value={review.previous as unknown as Json}/>
        <h4>{t.newResolution}</h4><BoundedRecord value={review.resolution as unknown as Json}/>
        <button id="target-reviewed-save" type="button" className="primary" disabled={disabled || !parsed.configuration} onClick={() => handlers.save(review.resolution)}>{t.reviewedSave}</button>
      </section>}
      {observation && <details id="target-observation"><summary>{t.observation}{!currentObservation ? ` · ${t.earlier}` : ''}</summary>
        <dl className="run-identity"><dt>{t.configurationIdentity}</dt><dd><code>{observation.check.configuration_identity}</code></dd></dl>
        <BoundedRecord value={observation.check as unknown as Json}/></details>}
      {binding && <div className="button-row">
        {confirmRemove ? <div className="confirm-row" role="alertdialog" aria-labelledby="target-remove-confirm-text" onKeyDown={event => {if (event.key === 'Escape') {event.preventDefault(); closeRemoval(false);}}}>
          <span id="target-remove-confirm-text">{t.confirmRemove}</span>
          <button id="target-remove-confirm" type="button" className="danger-text" disabled={disabled} onClick={() => closeRemoval(true)}>{t.removeConfirmed}</button>
          <button ref={cancelRemove} type="button" onClick={() => closeRemoval(false)}>{t.cancel}</button>
        </div> : <button id="target-remove" ref={removeButton} type="button" className="danger-text" disabled={disabled} onClick={() => setConfirmRemove(true)}>{t.remove}</button>}
      </div>}
    </div>
  </section>;
}
