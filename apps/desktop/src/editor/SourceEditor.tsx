import {useImperativeHandle, useLayoutEffect, useRef, useState} from 'react';
import type {RefObject} from 'react';
import {Annotation, Compartment, EditorState, Prec, Transaction} from '@codemirror/state';
import {EditorView, drawSelection, highlightActiveLine, keymap, lineNumbers} from '@codemirror/view';
import {defaultHighlightStyle, indentUnit, syntaxHighlighting} from '@codemirror/language';
import {javascript} from '@codemirror/lang-javascript';
import {defaultKeymap} from '@codemirror/commands';
import {autocompletion, closeCompletion, completionKeymap, pickedCompletion, startCompletion} from '@codemirror/autocomplete';
import type {Completion, CompletionSource} from '@codemirror/autocomplete';
import type {EditInput, FileDraft, Snapshot, TextRange} from '../authoring.ts';
import {AUTHORING_NON_IMAGE_BYTES, AUTHORING_SOURCE_BYTES} from '../authoring.ts';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';
import type {CompletionContext, CompletionKey, CompletionResult} from './completion-types.ts';
import {SourcePositions} from './source-positions.ts';
import type {SourceChange} from './source-positions.ts';

export interface SourceEditorHandle {complete: () => void; focus: () => void; composing: () => boolean}
interface Props {
  draft: FileDraft & {text: string};
  reveal: number;
  readOnly: boolean;
  selection: RefObject<TextRange>;
  control: RefObject<SourceEditorHandle | null>;
  context: CompletionContext | null;
  completionUnavailable: boolean;
  request: (position: number, explicit: boolean) => Promise<CompletionResult | null>;
  accepts: (key: CompletionKey, source: string) => boolean;
  onCompletionRefused: () => void;
  onEdit: (next: Snapshot, before: TextRange, input: EditInput) => void;
  onRange: (range: TextRange) => void;
  onCompositionStart: (range: TextRange) => void;
  onCompositionEnd: () => void;
  onUndo: () => void; onRedo: () => void; onSave: () => void;
  onFind: () => void; onFindNext: (backward: boolean) => void;
}

const synchronize = Annotation.define<boolean>();
const inputKind = Annotation.define<EditInput>();

export default function SourceEditor(props: Props) {
  const locale = useLocale();
  const a = messages[locale].ui.authoring;
  const latest = useRef({props, a});
  latest.current = {props, a};
  const mount = useRef<HTMLDivElement>(null);
  const editor = useRef<EditorView | null>(null);
  const positions = useRef<SourcePositions | null>(null);
  const composing = useRef(false);
  const nativeInput = useRef<EditInput | null>(null);
  const settings = useRef(new Compartment());
  const autoTimer = useRef<number | undefined>(undefined);
  const explicit = useRef(false);
  const [caret, setCaret] = useState({line: 1, column: 1, lines: 1});

  function cancelAutomatic() {
    clearTimeout(autoTimer.current);
    autoTimer.current = undefined;
  }
  function complete(requested: boolean) {
    cancelAutomatic();
    const view = editor.current;
    if (!view || composing.current || view.composing || latest.current.props.readOnly || !latest.current.props.context) return;
    explicit.current = requested;
    closeCompletion(view);
    if (requested) view.focus();
    startCompletion(view);
  }
  useImperativeHandle(props.control, () => ({complete: () => complete(true), focus: () => editor.current?.focus(),
    composing: () => composing.current || editor.current?.composing === true}));

  function configuration() {
    const {props: current, a: labels} = latest.current;
    return [EditorState.readOnly.of(current.readOnly), EditorView.editable.of(!current.readOnly),
      EditorView.contentAttributes.of({id: 'authoring-editor', 'aria-label': labels.editorLabel(current.draft.path),
        'aria-describedby': 'authoring-editor-help authoring-completion-scope', 'aria-readonly': String(current.readOnly),
        'data-path': current.draft.path, 'data-draft-revision': String(current.draft.revision),
        spellcheck: 'false', autocapitalize: 'off', autocorrect: 'off', tabindex: '0'}),
      EditorState.phrases.of(labels.completionPhrases)];
  }

  useLayoutEffect(() => {
    if (!mount.current) return;
    const first = latest.current.props;
    positions.current = new SourcePositions(first.draft.text);
    let alive = true;
    const rangeOf = (view: EditorView): TextRange => {
      const range = view.state.selection.main;
      return {start: positions.current!.toSource(range.from), end: positions.current!.toSource(range.to)};
    };
    const track = (view: EditorView) => {
      latest.current.props.selection.current = rangeOf(view);
      const line = view.state.doc.lineAt(view.state.selection.main.head);
      setCaret({line: line.number, column: view.state.selection.main.head - line.from + 1, lines: view.state.doc.lines});
    };
    const source: CompletionSource = async context => {
      const current = latest.current.props;
      const mapping = positions.current!;
      if (!alive || composing.current || current.readOnly || current.context?.source !== mapping.source) return null;
      const position = context.pos;
      const result = await current.request(mapping.toSource(position), explicit.current);
      if (!alive || context.aborted || !result || composing.current || editor.current?.composing
        || positions.current !== mapping || editor.current?.state.selection.main.head !== position
        || !latest.current.props.accepts(result.key, mapping.source)) return null;
      const options: Completion[] = result.candidates.filter(candidate => Number.isInteger(candidate.from)
        && Number.isInteger(candidate.to) && candidate.from >= 0 && candidate.to >= candidate.from
        && candidate.to <= mapping.source.length
        && mapping.toSource(mapping.toEditor(candidate.from)) === candidate.from
        && mapping.toSource(mapping.toEditor(candidate.to)) === candidate.to).map(candidate => ({
        label: candidate.label,
        detail: candidate.detailOmitted ? latest.current.a.completionDetailOmitted : candidate.detail,
        apply(view, option) {
          const current = latest.current.props;
          if (!alive || composing.current || view.composing || current.readOnly || positions.current !== mapping
            || view.state.selection.main.head !== position || !current.accepts(result.key, mapping.source)) {
            closeCompletion(view);
            return;
          }
          const from = mapping.toEditor(candidate.from), to = mapping.toEditor(candidate.to);
          const inserted = view.state.toText(candidate.insertText);
          const next = mapping.apply([{from, to, insert: inserted.toString()}]);
          const size = new TextEncoder().encode(next.source).length;
          if (size > AUTHORING_SOURCE_BYTES || size + (current.context?.otherBytes ?? AUTHORING_NON_IMAGE_BYTES) > AUTHORING_NON_IMAGE_BYTES) {
            closeCompletion(view);
            current.onCompletionRefused();
            return;
          }
          view.dispatch({changes: {from, to, insert: inserted}, selection: {anchor: from + inserted.length},
            annotations: [Transaction.userEvent.of('input.complete'), pickedCompletion.of(option),
              inputKind.of({type: 'insertReplacementText', data: null, composing: false})]});
        },
      }));
      // Each option owns its exact worker span; CodeMirror must not reuse it after another edit.
      return options.length ? {from: position, to: position, options, filter: false} : null;
    };
    const command = (run: () => void) => () => {
      if (composing.current || editor.current?.composing) return false;
      run();
      return true;
    };
    const view = new EditorView({parent: mount.current, state: EditorState.create({
      doc: positions.current.document,
      selection: {anchor: positions.current.toEditor(first.draft.range.start), head: positions.current.toEditor(first.draft.range.end, 1)},
      extensions: [settings.current.of(configuration()), EditorState.allowMultipleSelections.of(false), EditorState.tabSize.of(2),
        indentUnit.of('  '), lineNumbers(), drawSelection(), highlightActiveLine(),
        javascript({typescript: /\.[cm]?tsx?$/i.test(first.draft.path), jsx: /\.[jt]sx$/i.test(first.draft.path)}),
        syntaxHighlighting(defaultHighlightStyle),
        EditorState.changeFilter.of(transaction => !transaction.docChanged || transaction.annotation(synchronize) === true || !latest.current.props.readOnly),
        autocompletion({override: [source], activateOnTyping: false, defaultKeymap: false, icons: false}),
        Prec.highest(keymap.of([
          {key: 'Mod-z', run: command(() => {if (!latest.current.props.readOnly) latest.current.props.onUndo();}), preventDefault: true},
          {key: 'Mod-Shift-z', run: command(() => {if (!latest.current.props.readOnly) latest.current.props.onRedo();}), preventDefault: true},
          {key: 'Mod-y', run: command(() => {if (!latest.current.props.readOnly) latest.current.props.onRedo();}), preventDefault: true},
          {key: 'Mod-s', run: command(() => latest.current.props.onSave()), preventDefault: true},
          {key: 'Mod-f', run: command(() => latest.current.props.onFind()), preventDefault: true},
          {key: 'Mod-g', run: command(() => latest.current.props.onFindNext(false)), preventDefault: true},
          {key: 'Mod-Shift-g', run: command(() => latest.current.props.onFindNext(true)), preventDefault: true},
          {key: 'Ctrl-Space', run: command(() => complete(true)), preventDefault: true},
          {key: 'Tab', run: view => {
            if (composing.current || view.composing || latest.current.props.readOnly) return false;
            view.dispatch({...view.state.replaceSelection('  '), annotations: [Transaction.userEvent.of('input'),
              inputKind.of({type: 'insertText', data: '  ', composing: false})]});
            return true;
          }},
          ...completionKeymap.filter(binding => binding.key !== 'Ctrl-Space').map(binding => ({...binding,
            run: binding.run ? (view: EditorView) => !composing.current && !view.composing && binding.run!(view) : undefined})),
        ])),
        keymap.of(defaultKeymap),
      ],
    }), dispatchTransactions(transactions, view) {
      const edits: {next: Snapshot; before: TextRange; input: EditInput}[] = [];
      for (const transaction of transactions) {
        if (!transaction.docChanged || transaction.annotation(synchronize)) continue;
        const old = positions.current!;
        const before = {start: old.toSource(transaction.startState.selection.main.from), end: old.toSource(transaction.startState.selection.main.to)};
        const changes: SourceChange[] = [];
        transaction.changes.iterChanges((from, to, _fromNew, _toNew, inserted) => changes.push({from, to, insert: inserted.toString()}));
        positions.current = old.apply(changes);
        const selection = transaction.newSelection.main;
        const annotation = transaction.annotation(inputKind);
        const isComposition = composing.current || transaction.isUserEvent('input.type.compose');
        let input: EditInput;
        if (annotation) input = annotation;
        else if (isComposition) input = {type: 'insertCompositionText', data: null, composing: true};
        else if (transaction.isUserEvent('input.paste')) input = {type: 'insertFromPaste', data: null, composing: false};
        else if (transaction.isUserEvent('input.type')) input = nativeInput.current ?? {type: 'insertText', data: changes.map(change => change.insert).join(''), composing: false};
        else if (transaction.isUserEvent('delete.backward')) input = {type: 'deleteContentBackward', data: null, composing: false};
        else if (transaction.isUserEvent('delete.forward')) input = {type: 'deleteContentForward', data: null, composing: false};
        else input = {type: '', data: null, composing: false};
        nativeInput.current = null;
        edits.push({next: {text: positions.current.source, start: positions.current.toSource(selection.from), end: positions.current.toSource(selection.to)}, before, input});
      }
      view.update(transactions);
      if (transactions.some(transaction => transaction.docChanged || transaction.selection)) track(view);
      for (const edit of edits) latest.current.props.onEdit(edit.next, edit.before, edit.input);
      if (edits.length === 0 && transactions.some(transaction => transaction.selection && !transaction.annotation(synchronize))) {
        latest.current.props.onRange(rangeOf(view));
      }
      const changed = transactions.some(transaction => transaction.docChanged || transaction.selection);
      if (changed) {
        cancelAutomatic();
        closeCompletion(view);
      }
      if (edits.length > 0 && !composing.current && !view.composing) {
        autoTimer.current = window.setTimeout(() => {autoTimer.current = undefined; if (alive && view.hasFocus) complete(false);}, 100);
      }
    }});
    editor.current = view;
    let compositionTimer: number | undefined;
    const compositionEnd = () => {
      window.clearTimeout(compositionTimer);
      compositionTimer = undefined;
      if (!composing.current) return;
      composing.current = false;
      latest.current.props.onCompositionEnd();
    };
    const beforeInput = (event: InputEvent) => {
      if (event.inputType === 'historyUndo' || event.inputType === 'historyRedo') {
        event.preventDefault();
        event.stopImmediatePropagation();
        if (!composing.current && !view.composing && !latest.current.props.readOnly) {
          if (event.inputType === 'historyUndo') latest.current.props.onUndo(); else latest.current.props.onRedo();
        }
        return;
      }
      if (composing.current && !view.compositionStarted) compositionEnd();
      nativeInput.current = {type: event.inputType, data: event.data, composing: event.isComposing};
      if (composing.current && event.inputType === 'insertText' && !event.isComposing) {
        window.clearTimeout(compositionTimer);
        // WebKit can omit compositionend; CodeMirror repairs its own state first.
        compositionTimer = window.setTimeout(() => {
          compositionTimer = undefined;
          if (alive && !view.compositionStarted) compositionEnd();
        }, 50);
      }
    };
    const compositionStart = () => {
      if (latest.current.props.readOnly || composing.current) return;
      composing.current = true;
      cancelAutomatic();
      closeCompletion(view);
      latest.current.props.onCompositionStart(rangeOf(view));
    };
    const blur = () => latest.current.props.onRange(rangeOf(view));
    view.contentDOM.addEventListener('beforeinput', beforeInput, true);
    view.contentDOM.addEventListener('compositionstart', compositionStart, true);
    view.contentDOM.addEventListener('compositionend', compositionEnd, true);
    view.contentDOM.addEventListener('blur', blur);
    track(view);
    view.focus();
    return () => {
      alive = false;
      cancelAutomatic();
      window.clearTimeout(compositionTimer);
      view.contentDOM.removeEventListener('beforeinput', beforeInput, true);
      view.contentDOM.removeEventListener('compositionstart', compositionStart, true);
      view.contentDOM.removeEventListener('compositionend', compositionEnd, true);
      view.contentDOM.removeEventListener('blur', blur);
      // Leaving the view must not strand a draft in composition mode.
      if (composing.current) {composing.current = false; first.onCompositionEnd();}
      editor.current = null;
      view.destroy();
    };
  }, [props.draft.path]);

  useLayoutEffect(() => {
    const view = editor.current;
    if (!view) return;
    view.dispatch({effects: settings.current.reconfigure(configuration())});
  }, [props.readOnly, props.draft.revision, locale]);

  useLayoutEffect(() => {
    const view = editor.current;
    if (!view || positions.current?.source === props.draft.text) return;
    positions.current = new SourcePositions(props.draft.text);
    view.dispatch({changes: {from: 0, to: view.state.doc.length, insert: positions.current.document},
      selection: {anchor: positions.current.toEditor(props.draft.range.start), head: positions.current.toEditor(props.draft.range.end, 1)},
      annotations: synchronize.of(true)});
  }, [props.draft.text]);

  useLayoutEffect(() => {
    const view = editor.current, mapping = positions.current;
    if (!view || !mapping) return;
    const anchor = mapping.toEditor(props.draft.range.start), head = mapping.toEditor(props.draft.range.end, 1);
    view.dispatch({selection: {anchor, head}, effects: EditorView.scrollIntoView(head, {y: 'nearest'}), annotations: synchronize.of(true)});
    view.focus();
  }, [props.reveal, props.draft.path]);

  useLayoutEffect(() => {
    if (editor.current) closeCompletion(editor.current);
  }, [props.context]);

  useLayoutEffect(() => {
    if (props.completionUnavailable && editor.current) closeCompletion(editor.current);
  }, [props.completionUnavailable]);

  return <div className="code-editor">
    <div ref={mount} className="source-editor"/>
    <div className="editor-status"><span id="authoring-caret">{a.position(caret.line, caret.column)}</span><span>{a.lines(caret.lines)}</span>
      <span id="authoring-editor-help">{a.editorHelp}</span></div>
  </div>;
}
