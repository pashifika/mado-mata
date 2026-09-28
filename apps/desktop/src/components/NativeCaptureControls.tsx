import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';
import {nativePrimaryAction} from '../recognition.ts';
import type {NativeSelectionView} from '../types.ts';

export interface NativeCaptureControlsProps {
  disabled:boolean; cancelDisabled:boolean;
  selection:NativeSelectionView|null;
  onStart:() => void;
  onSelect:() => void;
  onCapture:(newCapture:boolean) => void;
  onCancel:() => void;
}

export default function NativeCaptureControls({disabled, cancelDisabled, selection, onStart, onSelect, onCapture, onCancel}:NativeCaptureControlsProps) {
  const locale = useLocale();
  const copy = messages[locale].ui.nativeCapture;
  const operation = selection?.busy === true;
  const selected = Boolean(selection?.occupied && selection.selected_id);
  const primary = nativePrimaryAction(selection);
  const unavailable = disabled || selection?.platform === 'unsupported';
  const label = selected ? copy.newCapture : copy.select;
  return <div className="preview-capture-button" role="group" aria-label={copy.capture} data-busy={operation}>
    <button id={selected ? 'native-new-capture' : 'native-select-window'} className="capture-icon-button" type="button"
      aria-label={label} title={label} disabled={unavailable || operation} onClick={selected ? () => onCapture(true) : onSelect}>
      <svg className="capture-control-icon" viewBox="0 0 20 20" aria-hidden="true" focusable="false">
        {selected ? <>
          <path d="M4 13H3a1 1 0 0 1-1-1V3a1 1 0 0 1 1-1h9a1 1 0 0 1 1 1v1"/>
          <rect x="6" y="6" width="12" height="12" rx="2"/>
          <path d="M12 9v6m-3-3h6"/>
        </> : <>
          <path d="M7 3H4a1 1 0 0 0-1 1v3m10-4h3a1 1 0 0 1 1 1v3M3 13v3a1 1 0 0 0 1 1h3m10-4v3a1 1 0 0 1-1 1h-3"/>
          <circle cx="10" cy="10" r="3"/>
        </>}
      </svg>
    </button>
    <button id={operation ? 'native-cancel' : 'native-capture'} className="capture-main-button" type="button"
      disabled={operation ? cancelDisabled : unavailable || primary === 'select'}
      title={operation ? copy.cancel : primary === 'start' ? copy.start : copy.capture}
      onClick={operation ? onCancel : primary === 'start' ? onStart : () => onCapture(false)}>
      {operation ? messages[locale].ui.recognition.stop : primary === 'start' ? copy.start : copy.capture}
    </button>
  </div>;
}
