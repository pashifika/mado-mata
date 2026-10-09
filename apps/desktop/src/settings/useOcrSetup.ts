import {useEffect, useRef, useState} from 'react';
import {invoke} from '@tauri-apps/api/core';
import {INITIAL_SETUP, OcrSetupSession} from './ocr-setup.ts';
import type {SetupClient, SetupOperation, SetupState, SetupView} from './ocr-setup.ts';
import type {SettingsDraft} from '../state.ts';
import type {OcrEnvironment} from '../types.ts';

const client:SetupClient = {
  catalog:() => invoke<SetupView>('ocr_setup_catalog'),
  start:(resourceId, environment) => invoke<string>('ocr_setup_start', {resourceId, environment}),
  poll:() => invoke<SetupOperation|null>('ocr_setup_poll'),
  cancel:operationId => invoke<void>('ocr_setup_cancel', {operationId}),
  openLink:(resourceId, index) => invoke<void>('ocr_setup_open_link', {resourceId, index}),
  copyCommand:(resourceId, index) => invoke<void>('ocr_setup_copy_command', {resourceId, index}),
};

export interface OcrSetupControls {
  state:SetupState; busy:boolean; stale:boolean; proposal:OcrEnvironment|null;
  reload:() => void; start:(resourceId:string) => void; recheck:() => void; cancel:() => void;
  guidance:(kind:'link'|'copy', resourceId:string, index:number) => void;
  use:() => void; close:() => void;
}

export function useOcrSetup(open:boolean, draft:SettingsDraft, onDraft:(next:SettingsDraft) => void):OcrSetupControls {
  const [state, setState] = useState<SetupState>(INITIAL_SETUP);
  const sessionRef = useRef<OcrSetupSession|null>(null);
  const draftRef = useRef(draft);
  draftRef.current = draft;
  sessionRef.current?.observeDraft(draft);

  useEffect(() => {
    if (!open) return;
    const session = new OcrSetupSession(client, draftRef.current, setState);
    sessionRef.current = session;
    setState(session.snapshot);
    void session.load();
    return () => {session.close(); if (sessionRef.current === session) sessionRef.current = null;};
  }, [open]);

  useEffect(() => {
    if (!open || state.id === null || state.operation?.active === false) return;
    const session = sessionRef.current;
    if (session === null) return;
    let alive = true;
    let timer:number|undefined;
    const tick = async () => {
      await session.poll();
      if (alive) timer = window.setTimeout(tick, 300);
    };
    void tick();
    return () => {alive = false; window.clearTimeout(timer);};
  }, [open, state.id, state.operation?.active]);

  const session = sessionRef.current;
  return {
    state,
    busy:open && (state.cancelPending || state.starting !== null || Boolean(state.operation?.cleanup_error) || (state.id !== null && state.operation?.active !== false)),
    stale:state.checkedRevision !== null && session !== null && state.checkedRevision !== session.draftRevision,
    proposal:open ? session?.currentEnvironment(draft) ?? null : null,
    reload:() => {void sessionRef.current?.load();},
    start:resourceId => {void sessionRef.current?.start(resourceId, draftRef.current);},
    recheck:() => {void sessionRef.current?.start('inspect', draftRef.current);},
    cancel:() => {void sessionRef.current?.cancel();},
    guidance:(kind, resourceId, index) => {void sessionRef.current?.guidance(kind, resourceId, index);},
    use:() => {
      const next = sessionRef.current?.apply(draftRef.current);
      if (next) onDraft(next);
    },
    close:() => {sessionRef.current?.close();},
  };
}
