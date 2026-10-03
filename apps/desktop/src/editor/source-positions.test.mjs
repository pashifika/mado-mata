import {test} from 'node:test';
import assert from 'node:assert/strict';
import {EditorState} from '@codemirror/state';
import {editFile, fileDirty, findMatch, offsetAt, openSession, undoFile} from '../authoring.ts';
import {SourcePositions} from './source-positions.ts';

function transaction(source, spec) {
  const mapping = new SourcePositions(source);
  const edit = EditorState.create({doc: mapping.document}).update(spec);
  const changes = [];
  edit.changes.iterChanges((from, to, _fromNew, _toNew, inserted) => changes.push({from, to, insert: inserted.toString()}));
  return {mapping: mapping.apply(changes), edit};
}

test('mixed separators and UTF-16 positions round-trip without rewriting the source', () => {
  const source = '先頭😀\r\nlet x = "あ";\r次\n終😀';
  const mapping = new SourcePositions(source);
  assert.equal(mapping.document, '先頭😀\nlet x = "あ";\n次\n終😀');
  for (let position = 0; position <= mapping.document.length; position += 1) {
    assert.equal(mapping.toEditor(mapping.toSource(position)), position);
  }
  const newline = source.indexOf('\r');
  assert.equal(mapping.toEditor(newline + 1, -1), newline);
  assert.equal(mapping.toEditor(newline + 1, 1), newline + 1);
  assert.equal(mapping.toSource(newline + 1), newline + 2);
  assert.equal(mapping.source, source);
  assert.equal(mapping.apply([]), mapping);
});

test('normalized no-op replacement preserves every original separator', () => {
  const source = 'a\r\nb\rc\nd\r\n';
  const mapping = new SourcePositions(source);
  const result = mapping.apply([{from: 0, to: mapping.document.length, insert: mapping.document}]);
  assert.equal(result.source, source);
  assert.equal(result, mapping);
});

test('one multi-change editor transaction preserves untouched mixed separators', () => {
  const {mapping, edit} = transaction('a\r\nb\rc\nd\r\n', {
    changes: [{from: 2, to: 3, insert: 'BB'}, {from: 4, to: 5, insert: 'CCC'}, {from: 6, to: 7, insert: 'Z'}],
  });
  assert.equal(mapping.source, 'a\r\nBB\rCCC\nZ\r\n');
  assert.equal(mapping.document, edit.newDoc.toString());
});

test('inserted newlines use the current line separator and EOF inherits the preceding line', () => {
  const {mapping} = transaction('a\r\nb\rc\nlast', {changes: {from: 1, insert: '\n日本\n'}});
  assert.equal(mapping.source, 'a\r\n日本\r\n\r\nb\rc\nlast');
  const loneCR = transaction('a\r\nb\rc\nlast', {changes: {from: 3, insert: '\n'}}).mapping;
  assert.equal(loneCR.source, 'a\r\nb\r\rc\nlast');
  const eof = transaction('a\r\nb', {changes: {from: 3, insert: '\n😀'}}).mapping;
  assert.equal(eof.source, 'a\r\nb\r\n😀');
  assert.equal(eof.toSource(eof.document.length), eof.source.length);
  assert.equal(transaction('single', {changes: {from: 6, insert: '\n'}}).mapping.source, 'single\n');
});

test('deleting a visible CRLF or a non-BMP character never leaves half a source separator', () => {
  assert.equal(transaction('前😀\r\n後\r終', {changes: {from: 3, to: 4}}).mapping.source, '前😀後\r終');
  assert.equal(transaction('前😀\r\n後\r終', {changes: {from: 1, to: 3}}).mapping.source, '前\r\n後\r終');
});

test('source search and diagnostic spans edit the intended Unicode text and undo restores exact saved bytes', () => {
  const source = '前😀\r\nhost.options.旧\r次\n終';
  const positions = new SourcePositions(source);
  const match = findMatch(source, '旧', {start: 0, end: 0}, false);
  assert.ok(match);
  assert.equal(positions.toEditor(offsetAt(source, 2, 14)), positions.document.indexOf('旧'));
  const from = positions.toEditor(match.start), to = positions.toEditor(match.end);
  const {mapping, edit} = transaction(source, {changes: {from, to, insert: '新😀'}, selection: {anchor: from + 3}});
  assert.equal(mapping.source, '前😀\r\nhost.options.新😀\r次\n終');
  const caret = mapping.toSource(edit.newSelection.main.head);
  assert.equal(caret, mapping.source.indexOf('\r次'));
  const owner = {workspace: {workspace_id: 'mapping', revision: 1}, token: 'mapping-lease'};
  let session = openSession({owner, package_path: '/mapping', package_id: 'mapping', revision: 'saved',
    files: [{path: 'main.ts', kind: 'source', text: source, bytes: new TextEncoder().encode(source).length}]});
  session = editFile(session, 'main.ts', {text: mapping.source, start: caret, end: caret}, match,
    {type: 'insertReplacementText', data: null, composing: false});
  assert.equal(fileDirty(session.drafts.get('main.ts')), true);
  session = undoFile(session, 'main.ts');
  assert.equal(session.drafts.get('main.ts').text, source);
  assert.deepEqual(session.drafts.get('main.ts').range, match);
  assert.equal(fileDirty(session.drafts.get('main.ts')), false);
});

test('invalid or overlapping editor edits are refused rather than clipping source', () => {
  const mapping = new SourcePositions('a\r\nb');
  assert.throws(() => mapping.apply([{from: -1, to: 1, insert: ''}]), RangeError);
  assert.throws(() => mapping.apply([{from: 0, to: 4, insert: ''}]), RangeError);
  assert.throws(() => mapping.apply([{from: 0, to: 2, insert: ''}, {from: 1, to: 3, insert: ''}]), RangeError);
  assert.equal(mapping.source, 'a\r\nb');
});
