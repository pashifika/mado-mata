import type {Transaction} from '@codemirror/state';

export function activatesCompletion(transaction: Transaction): boolean {
  if (!transaction.docChanged || !transaction.isUserEvent('input.type')
    || transaction.isUserEvent('input.type.compose') || !transaction.newSelection.main.empty) return false;
  const position = transaction.newSelection.main.head;
  return /[$\p{ID_Continue}.'"`]$/u.test(transaction.newDoc.sliceString(Math.max(0, position - 2), position));
}
