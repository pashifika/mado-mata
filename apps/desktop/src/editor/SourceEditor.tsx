import {useImperativeHandle, useLayoutEffect, useRef, useState} from 'react';
import type {RefObject} from 'react';
import {Annotation, Compartment, EditorState, Prec, Transaction} from '@codemirror/state';
import {EditorView, drawSelection, highlightActiveLine, keymap, lineNumbers, tooltips} from '@codemirror/view';
import type {Rect} from '@codemirror/view';
import {defaultHighlightStyle, indentUnit, syntaxHighlighting} from '@codemirror/language';
import {javascript} from '@codemirror/lang-javascript';
import {defaultKeymap} from '@codemirror/commands';
import {acceptCompletion, autocompletion, closeCompletion, completionKeymap, pickedCompletion, selectedCompletion, startCompletion} from '@codemirror/autocomplete';
import type {Completion, CompletionSource} from '@codemirror/autocomplete';
import type {EditInput, FileDraft, Snapshot, TextRange} from '../authoring.ts';
import type {EditorCompletionPreferences} from '../types.ts';
import {AUTHORING_NON_IMAGE_BYTES, AUTHORING_SOURCE_BYTES} from '../authoring.ts';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';
import type {CompletionCandidate, CompletionContext, CompletionKey, CompletionResult} from './completion-types.ts';
import {SourcePositions} from './source-positions.ts';
import {CompletionSession} from './completion-session.ts';
import type {SourceChange} from './source-positions.ts';

export interface SourceEditorHandle {complete: () => void; focus: () => void; composing: () => boolean}
interface Props {
  draft: FileDraft & {text: string};
  reveal: number;
  readOnly: boolean;
  topInset: number;
  completionPreferences: EditorCompletionPreferences;
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

// Adapted from VS Code's MyCompletionItem.convertKind. See public/third-party/vscode-icons/NOTICE.txt.
function completionType(kind: string): string {
  switch (kind) {
    case 'primitive type': case 'keyword': return 'keyword';
    case 'const': case 'let': case 'var': case 'local var': case 'alias': case 'parameter': return 'variable';
    case 'property': case 'getter': case 'setter': return 'field';
    case 'function': case 'local function': return 'function';
    case 'method': case 'construct': case 'call': case 'index': return 'method';
    case 'enum': return 'enum';
    case 'enum member': return 'enum-member';
    case 'module': case 'external module name': return 'module';
    case 'class': case 'type': return 'class';
    case 'interface': return 'interface';
    case 'warning': return 'text';
    case 'script': return 'file';
    case 'directory': return 'folder';
    case 'string': return 'constant';
    default: return 'property';
  }
}

function completionInfo(candidate: CompletionCandidate, omitted: string) {
  if (!candidate.detail && !candidate.documentation && !candidate.detailOmitted) return null;
  const panel = document.createElement('div');
  panel.className = 'source-completion-content';
  panel.addEventListener('mousedown', event => event.preventDefault());
  if (candidate.detail) {
    const signature = document.createElement('pre');
    signature.className = 'source-completion-signature';
    signature.textContent = candidate.detail;
    panel.appendChild(signature);
  }
  if (candidate.documentation) {
    const documentation = document.createElement('div');
    documentation.className = 'source-completion-documentation';
    documentation.textContent = candidate.documentation;
    panel.appendChild(documentation);
  }
  if (candidate.detailOmitted) {
    const notice = document.createElement('p');
    notice.className = 'source-completion-omitted';
    notice.textContent = omitted;
    panel.appendChild(notice);
  }
  return panel;
}

function navigationClearance(view: EditorView): number {
  const shell = view.dom.closest('.app');
  return shell ? parseFloat(getComputedStyle(shell).getPropertyValue('--navigation-clearance')) || 0 : 0;
}

function completionSpace(view: EditorView): Rect {
  const viewport = view.dom.ownerDocument.documentElement;
  const editor = view.scrollDOM.getBoundingClientRect();
  const space = {left: Math.max(0, editor.left), top: Math.max(navigationClearance(view), editor.top),
    right: Math.min(viewport.clientWidth, editor.right), bottom: Math.min(viewport.clientHeight, editor.bottom)};
  for (let parent = view.scrollDOM.parentElement; parent; parent = parent.parentElement) {
    const style = getComputedStyle(parent);
    const clipX = /auto|scroll|hidden|clip/.test(style.overflowX);
    const clipY = /auto|scroll|hidden|clip/.test(style.overflowY);
    if (!clipX && !clipY) continue;
    const bounds = parent.getBoundingClientRect();
    if (clipX) {space.left = Math.max(space.left, bounds.left); space.right = Math.min(space.right, bounds.right);}
    if (clipY) {space.top = Math.max(space.top, bounds.top); space.bottom = Math.min(space.bottom, bounds.bottom);}
  }
  return space;
}

function positionCompletionInfo(view: EditorView, list: Rect, option: Rect, info: Rect, space: Rect) {
  const scaleX = view.scaleX, scaleY = view.scaleY;
  const gapX = 8 * scaleX, gapY = 8 * scaleY;
  const left = space.left + 8, right = space.right - 8;
  const top = space.top + 8, bottom = space.bottom - 8;
  let width = Math.min(416 * scaleX, Math.max(0, right - left));
  const height = Math.min(info.bottom - info.top, Math.max(0, bottom - top));
  const spaceRight = right - list.right - gapX;
  let x: number, offset: string, maxHeight: number, placement: string;
  if (spaceRight >= Math.min(width, 280 * scaleX)) {
    width = Math.min(width, spaceRight);
    placement = 'right';
    x = list.right + gapX;
    const y = Math.max(top, Math.min(option.top, bottom - height));
    offset = `top: ${(y - list.top) / scaleY}px`;
    maxHeight = bottom - y;
  } else {
    x = Math.max(left, Math.min(list.left, right - width));
    const below = Math.max(0, bottom - list.bottom - gapY);
    const above = Math.max(0, list.top - gapY - top);
    // Keep the panel outside the entire list, even when the selected row is in its middle.
    if (below < Math.min(height, 96 * scaleY) && above > below) {
      placement = 'above';
      offset = `bottom: ${(list.bottom - list.top + gapY) / scaleY}px`;
      maxHeight = above;
    } else {
      placement = 'below';
      offset = `top: ${(list.bottom - list.top + gapY) / scaleY}px`;
      maxHeight = below;
    }
  }
  return {
    style: `left: ${(x - list.left) / scaleX}px; ${offset}; width: ${width / scaleX}px; max-width: ${width / scaleX}px; max-height: ${Math.min(320, maxHeight / scaleY)}px`,
    class: `source-completion-panel-${placement}`,
  };
}

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
  const completion = useRef<CompletionSession | null>(null);
  const [caret, setCaret] = useState({line: 1, column: 1, lines: 1});
  const previousInset = useRef(0);

  useImperativeHandle(props.control, () => ({complete: () => {
    const view = editor.current;
    if (!view || composing.current || view.composing || latest.current.props.readOnly) return;
    view.focus();
    view.dom.scrollIntoView({block: 'nearest', inline: 'nearest'});
    completion.current?.explicit();
  }, focus: () => editor.current?.focus(), composing: () => composing.current || editor.current?.composing === true}));

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
    let session: CompletionSession | null = null;
    const navigationScroll = {};
    const rangeOf = (view: EditorView): TextRange => {
      const range = view.state.selection.main;
      return {start: positions.current!.toSource(range.from), end: positions.current!.toSource(range.to)};
    };
    const track = (view: EditorView) => {
      latest.current.props.selection.current = rangeOf(view);
      const line = view.state.doc.lineAt(view.state.selection.main.head);
      setCaret({line: line.number, column: view.state.selection.main.head - line.from + 1, lines: view.state.doc.lines});
    };
    const source: CompletionSource = context => {
      const publication = session?.current();
      // Only our session can publish. In particular, CodeMirror's composition-end trigger cannot.
      if (!context.explicit || !publication) return null;
      const {result, ticket} = publication;
      const mapping = ticket.snapshot.mapping;
      const position = ticket.snapshot.state.selection.main.head;
      if (context.pos !== position || context.state.doc !== ticket.snapshot.state.doc) return null;
      const options: Completion[] = result.candidates.filter(candidate => Number.isInteger(candidate.from)
        && Number.isInteger(candidate.to) && candidate.from >= 0 && candidate.to >= candidate.from
        && candidate.to <= mapping.source.length
        && mapping.toSource(mapping.toEditor(candidate.from)) === candidate.from
        && mapping.toSource(mapping.toEditor(candidate.to)) === candidate.to).map(candidate => ({
        label: candidate.label,
        type: completionType(candidate.kind),
        info: () => session?.accepts(publication) ? completionInfo(candidate, latest.current.a.completionDetailOmitted) : null,
        apply(view, option) {
          const current = latest.current.props;
          if (!alive || !session?.accepts(publication)) {
            session?.cancel();
            return;
          }
          const from = mapping.toEditor(candidate.from), to = mapping.toEditor(candidate.to);
          const inserted = view.state.toText(candidate.insertText);
          const next = mapping.apply([{from, to, insert: inserted.toString()}]);
          const size = new TextEncoder().encode(next.source).length;
          if (size > AUTHORING_SOURCE_BYTES || size + (current.context?.otherBytes ?? AUTHORING_NON_IMAGE_BYTES) > AUTHORING_NON_IMAGE_BYTES) {
            session?.cancel();
            current.onCompletionRefused();
            return;
          }
          session.cancel();
          view.dispatch({changes: {from, to, insert: inserted}, selection: {anchor: from + inserted.length}, scrollIntoView: true,
            annotations: [Transaction.userEvent.of('input.complete'), pickedCompletion.of(option),
              inputKind.of({type: 'insertReplacementText', data: null, composing: false})]});
        },
      }));
      // Each option owns its exact worker span; CodeMirror must not reuse it after another edit.
      if (!options.length) { session?.cancel(); return null; }
      return {from: position, to: position, options, filter: false};
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
        EditorView.scrollMargins.of(() => ({top: latest.current.props.topInset})),
        EditorView.scrollHandler.of((view, range, options) => {
          // Let CodeMirror reveal inside its scroller first; then clear the fixed shell.
          view.requestMeasure({
            key: navigationScroll,
            read: () => {
              const shell = view.dom.closest('.app');
              const style = shell ? getComputedStyle(shell) : null;
              const top = parseFloat(style?.getPropertyValue('--navigation-clearance') ?? '') || 0;
              const bottom = view.dom.ownerDocument.documentElement.clientHeight
                - (parseFloat(style?.getPropertyValue('--status-height') ?? '') || 0)
                - (parseFloat(style?.getPropertyValue('--drawer-height') ?? '') || 0);
              const bounds = view.scrollDOM.getBoundingClientRect();
              const caret = view.coordsAtPos(range.head, range.assoc || (range.head > range.anchor ? -1 : 1));
              if (!caret || caret.bottom <= bounds.top || caret.top >= bounds.bottom) return 0;
              const margin = Math.max(0, options.yMargin);
              return caret.top < top ? caret.top - top - margin : caret.bottom > bottom ? caret.bottom - bottom + margin : 0;
            },
            write: delta => {if (delta !== 0) view.dom.ownerDocument.defaultView?.scrollBy(0, delta);},
          });
          return false;
        }),
        EditorState.changeFilter.of(transaction => !transaction.docChanged || transaction.annotation(synchronize) === true || !latest.current.props.readOnly),
        tooltips({tooltipSpace: completionSpace}),
        autocompletion({override: [source], activateOnTyping: false, defaultKeymap: false, icons: true,
          positionInfo: positionCompletionInfo}),
        Prec.highest(keymap.of([
          {key: 'Mod-z', run: command(() => {if (!latest.current.props.readOnly) latest.current.props.onUndo();}), preventDefault: true},
          {key: 'Mod-Shift-z', run: command(() => {if (!latest.current.props.readOnly) latest.current.props.onRedo();}), preventDefault: true},
          {key: 'Mod-y', run: command(() => {if (!latest.current.props.readOnly) latest.current.props.onRedo();}), preventDefault: true},
          {key: 'Mod-s', run: command(() => latest.current.props.onSave()), preventDefault: true},
          {key: 'Mod-f', run: command(() => latest.current.props.onFind()), preventDefault: true},
          {key: 'Mod-g', run: command(() => latest.current.props.onFindNext(false)), preventDefault: true},
          {key: 'Mod-Shift-g', run: command(() => latest.current.props.onFindNext(true)), preventDefault: true},
          {key: 'Ctrl-Space', run: command(() => session?.explicit()), preventDefault: true},
          {key: 'Escape', run: view => {
            if (composing.current || view.composing) return false;
            return session?.cancel() ?? false;
          }},
          {key: 'Tab', run: view => {
            if (composing.current || view.composing || latest.current.props.readOnly) return false;
            if (selectedCompletion(view.state)) {
              acceptCompletion(view);
              return true;
            }
            view.dispatch({...view.state.replaceSelection('  '), annotations: [Transaction.userEvent.of('input'),
              inputKind.of({type: 'insertText', data: '  ', composing: false})]});
            return true;
          }},
          ...completionKeymap.filter(binding => binding.run !== startCompletion && binding.key !== 'Escape').map(binding => ({...binding,
            run: binding.run ? (view: EditorView) => !composing.current && !view.composing && binding.run!(view) : undefined})),
        ])),
        keymap.of(defaultKeymap),
      ],
    }), dispatchTransactions(transactions, view) {
      const edits: {next: Snapshot; before: TextRange; input: EditInput}[] = [];
      const inputs: (EditInput | null)[] = [];
      for (const transaction of transactions) {
        if (!transaction.docChanged || transaction.annotation(synchronize)) { inputs.push(null); continue; }
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
        inputs.push(input);
        nativeInput.current = null;
        edits.push({next: {text: positions.current.source, start: positions.current.toSource(selection.from), end: positions.current.toSource(selection.to)}, before, input});
      }
      view.update(transactions);
      if (transactions.some(transaction => transaction.docChanged || transaction.selection)) track(view);
      session?.update(transactions, inputs);
      for (const edit of edits) latest.current.props.onEdit(edit.next, edit.before, edit.input);
      if (edits.length === 0 && transactions.some(transaction => transaction.selection && !transaction.annotation(synchronize))) {
        latest.current.props.onRange(rangeOf(view));
      }
    }});
    editor.current = view;
    session = new CompletionSession({
      read: () => ({state: view.state, mapping: positions.current!, context: latest.current.props.context,
        focused: view.hasFocus, composing: composing.current || view.composing,
        readOnly: latest.current.props.readOnly, preferences: latest.current.props.completionPreferences}),
      request: (position, explicit) => latest.current.props.request(position, explicit),
      accepts: (result, source) => alive && latest.current.props.accepts(result.key, source),
      close: () => { closeCompletion(view); },
      publish: () => {
        view.dispatch({effects: EditorView.scrollIntoView(view.state.selection.main.head)});
        startCompletion(view);
      },
      setTimer: (callback, ms) => window.setTimeout(callback, ms),
      clearTimer: timer => window.clearTimeout(timer),
    });
    completion.current = session;
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
      session?.cancel();
      latest.current.props.onCompositionStart(rangeOf(view));
    };
    const blur = () => {
      session?.cancel();
      latest.current.props.onRange(rangeOf(view));
    };
    view.contentDOM.addEventListener('beforeinput', beforeInput, true);
    view.contentDOM.addEventListener('compositionstart', compositionStart, true);
    view.contentDOM.addEventListener('compositionend', compositionEnd, true);
    view.contentDOM.addEventListener('blur', blur);
    track(view);
    view.focus();
    return () => {
      alive = false;
      session?.cancel();
      completion.current = null;
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
    const view = editor.current;
    if (!view) return;
    const scrollTop = view.scrollDOM.scrollTop;
    const delta = props.topInset - previousInset.current;
    previousInset.current = props.topInset;
    view.dom.style.setProperty('--source-top-inset', `${props.topInset}px`);
    if (scrollTop > 0) view.scrollDOM.scrollTop = scrollTop + delta;
    view.requestMeasure({
      key: previousInset,
      read: () => {
        const bounds = view.scrollDOM.getBoundingClientRect();
        const caret = view.coordsAtPos(view.state.selection.main.head);
        if (!caret || caret.top < bounds.top || caret.bottom > bounds.bottom) return 0;
        return Math.min(0, caret.top - bounds.top - latest.current.props.topInset - 8);
      },
      write: adjustment => {if (adjustment < 0) view.scrollDOM.scrollTop += adjustment;},
    });
  }, [props.topInset]);

  useLayoutEffect(() => {
    completion.current?.synchronize();
  }, [props.context, props.readOnly, props.completionPreferences.automatic, props.completionPreferences.delay_ms]);

  useLayoutEffect(() => {
    if (props.completionUnavailable) completion.current?.cancel();
  }, [props.completionUnavailable]);

  return <div className="code-editor">
    <div ref={mount} className="source-editor"/>
    <div className="editor-status"><span id="authoring-caret">{a.position(caret.line, caret.column)}</span><span>{a.lines(caret.lines)}</span>
      <span id="authoring-editor-help">{a.editorHelp}</span>
      <span id="authoring-completion-scope" className="visually-hidden">{a.completionScope}</span></div>
  </div>;
}
