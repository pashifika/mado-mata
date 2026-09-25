import {useEffect, useRef} from 'react';
import type {ReactNode} from 'react';

interface Props {
  id: string; open: boolean; onCancel: () => void; labelledBy: string; className?: string;
  // Escape and the close control are refused while a dispatched command owns the dialog.
  locked?: boolean;
  // Selector of the control that takes focus when the dialog opens; defaults to the first focusable field.
  initialFocus?: string;
  children: ReactNode;
}

// Native modal semantics shared by the creation and saved-workspace dialogs: the rest of the document is inert,
// Escape raises cancel, and focus returns to the opener when the dialog closes.
export default function Modal({id, open, onCancel, labelledBy, className, locked = false, initialFocus, children}: Props) {
  const dialog = useRef<HTMLDialogElement>(null);
  const opener = useRef<Element | null>(null);
  useEffect(() => {
    const element = dialog.current;
    if (!element) return;
    if (open && !element.open) {
      opener.current = document.activeElement;
      element.showModal();
      element.querySelector<HTMLElement>(initialFocus ?? 'input, select, textarea, button:not([disabled])')?.focus();
    } else if (!open && element.open) {
      element.close();
      const target = opener.current instanceof HTMLElement && opener.current.isConnected ? opener.current : document.getElementById('workspace-select');
      target?.focus();
    }
  }, [open, initialFocus]);
  return <dialog id={id} ref={dialog} className={className ? `modal ${className}` : 'modal'} aria-labelledby={labelledBy}
    onCancel={event => {event.preventDefault(); if (!locked) onCancel();}}>
    {open && children}
  </dialog>;
}
