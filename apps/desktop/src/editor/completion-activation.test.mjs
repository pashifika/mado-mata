import {test} from 'node:test';
import assert from 'node:assert/strict';
import {EditorState, Transaction} from '@codemirror/state';
import {activatesCompletion} from './completion-activation.ts';

const typingCases = [
  {scenario: 'identifier typing', before: '', insert: 'r', expected: true},
  {scenario: 'member trigger', before: 'host', insert: '.', expected: true},
  {scenario: 'method literal trigger', before: 'host.call(', insert: '"', expected: true},
  {scenario: 'non-BMP identifier typing', before: '', insert: '𐐀', expected: true},
  {scenario: 'whitespace after identifier', before: 'return', insert: ' ', expected: false},
  {scenario: 'newline after identifier', before: 'return', insert: '\n', expected: false},
  {scenario: 'statement delimiter', before: 'value', insert: ';', expected: false},
];
for (const {scenario, before, insert, expected} of typingCases) {
  test(`automatic completion follows ${scenario}`, () => {
    const state = EditorState.create({doc: before});
    const transaction = state.update({changes: {from: before.length, insert}, selection: {anchor: before.length + insert.length},
      annotations: Transaction.userEvent.of('input.type')});
    assert.equal(activatesCompletion(transaction), expected);
  });
}

const nonTypingCases = [
  {scenario: 'completion acceptance', event: 'input.complete'},
  {scenario: 'paste', event: 'input.paste'},
  {scenario: 'drop', event: 'input.drop'},
  {scenario: 'composition update', event: 'input.type.compose'},
  {scenario: 'Undo', event: 'undo'},
  {scenario: 'Redo', event: 'redo'},
  {scenario: 'replacement', event: 'input'},
  {scenario: 'programmatic synchronization', event: undefined},
];
for (const {scenario, event} of nonTypingCases) {
  test(`automatic completion stays closed after ${scenario}`, () => {
    const state = EditorState.create({doc: 'old'});
    const transaction = state.update({changes: {from: 0, to: 3, insert: 'r'}, selection: {anchor: 1},
      annotations: event ? Transaction.userEvent.of(event) : []});
    assert.equal(activatesCompletion(transaction), false);
  });
}

test('automatic completion stays closed for selection movement and selected typing results', () => {
  const state = EditorState.create({doc: 'release'});
  assert.equal(activatesCompletion(state.update({selection: {anchor: 1}})), false);
  assert.equal(activatesCompletion(state.update({changes: {from: 0, insert: 'r'}, selection: {anchor: 0, head: 1},
    annotations: Transaction.userEvent.of('input.type')})), false);
});
