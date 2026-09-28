import {useState} from 'react';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';
import type {CaptureCacheReceipt, NativeSelectionView} from '../types.ts';
import {FaultMessage} from './ResultPanel.tsx';

export interface NativeCaptureControlsProps {
  disabled:boolean;
  selection:NativeSelectionView|null;
  cache:CaptureCacheReceipt|null;
  onDiscover:() => void;
  onExecutable:(path:string|null) => void;
  onSelect:() => void;
  onCandidate:(id:string) => void;
  onCapture:() => void;
  onCancel:() => void;
}

export default function NativeCaptureControls({disabled, selection, cache, onDiscover, onExecutable, onSelect, onCandidate, onCapture, onCancel}:NativeCaptureControlsProps) {
  const locale = useLocale();
  const copy = messages[locale].ui.nativeCapture;
  const [executable, setExecutable] = useState('');
  const windows = selection?.platform === 'windows';
  const unavailable = !selection || selection.platform === 'unsupported';
  const operation = selection?.status === 'discovering' || selection?.status === 'capturing' || selection?.status === 'cancelling';
  const discover = windows ? () => onExecutable(null) : onDiscover;
  return <section className="panel recognition-native" aria-labelledby="native-capture-heading">
    <h4 id="native-capture-heading">{copy.heading}</h4>
    <p>{copy.intro}</p>
    <p id="native-capture-status" role="status">{copy.status[selection?.status ?? 'unselected']}</p>
    {windows && <div className="field">
      <label htmlFor="native-executable">{copy.executable}</label>
      <input id="native-executable" type="text" value={executable} spellCheck={false} disabled={disabled || operation}
        onChange={event => setExecutable(event.target.value)}/>
      <button id="native-use-executable" type="button" disabled={disabled || operation || !executable.trim()} onClick={() => {
        const path = executable;
        setExecutable('');
        onExecutable(path);
      }}>{copy.useExecutable}</button>
    </div>}
    <div className="button-row">
      <button id="native-discover" type="button" disabled={disabled || unavailable || operation} onClick={discover}>
        {windows ? copy.browseExecutable : copy.discover}</button>
      <button id="native-select-window" type="button" disabled={disabled || operation || !selection?.candidates.length} onClick={onSelect}>{copy.select}</button>
      <button id="native-capture" type="button" disabled={disabled || selection?.status !== 'selected' || !selection.selected_id} onClick={onCapture}>{copy.capture}</button>
      <button id="native-refresh" type="button" disabled={disabled || unavailable || operation} onClick={discover}>{copy.refresh}</button>
      <button id="native-cancel" type="button" disabled={!selection?.occupied} onClick={onCancel}>{copy.cancel}</button>
    </div>
    {selection && selection.candidates.length > 0 && <fieldset id="native-candidates" disabled={disabled || operation}>
      <legend>{copy.candidates}</legend>
      {selection.candidates.map(candidate => <label key={candidate.id}>
        <input type="radio" name="native-window" value={candidate.id} checked={selection.selected_id === candidate.id}
          onChange={() => onCandidate(candidate.id)}/>
        {candidate.application_label} · {candidate.window_label}
      </label>)}
    </fieldset>}
    {selection?.error && <FaultMessage title={copy.status.failed} value={selection.error}/>}
    {cache && <div id="native-cache-result" role="status">
      <p>{cache.cached ? copy.cached : copy.notCached}</p>
      {cache.error && <FaultMessage title={copy.cacheFailed} value={cache.error}/>}
    </div>}
    <p className="field-help">{copy.selectionHelp}</p>
    <p className="field-help">{copy.permissionHelp}</p>
    <p className="field-help">{copy.scope}</p>
  </section>;
}
