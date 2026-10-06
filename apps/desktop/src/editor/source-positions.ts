export interface SourceChange {from: number; to: number; insert: string}
interface LineBreak {editor: number; text: string}

function lowerBound(values: readonly number[], position: number): number {
  let low = 0, high = values.length;
  while (low < high) {
    const middle = (low + high) >>> 1;
    if (values[middle] < position) low = middle + 1; else high = middle;
  }
  return low;
}

// CodeMirror counts every line break as one UTF-16 unit. The source keeps its original separators.
export class SourcePositions {
  readonly source: string;
  readonly document: string;
  private readonly breaks: LineBreak[] = [];
  private readonly sourceCRLF: number[] = [];
  private readonly editorCRLF: number[] = [];

  constructor(source: string) {
    this.source = source;
    this.document = source.replace(/\r\n|\r|\n/g, (text, position: number) => {
      const editor = position - this.sourceCRLF.length;
      this.breaks.push({editor, text});
      if (text === '\r\n') {
        this.sourceCRLF.push(position);
        this.editorCRLF.push(editor);
      }
      return '\n';
    });
  }

  toSource(position: number): number {
    const at = Math.max(0, Math.min(position, this.document.length));
    return at + lowerBound(this.editorCRLF, at);
  }

  // A source position inside CRLF must choose the preceding or following visible boundary.
  toEditor(position: number, association: -1 | 1 = -1): number {
    const at = Math.max(0, Math.min(position, this.source.length));
    const before = lowerBound(this.sourceCRLF, at);
    const inside = before > 0 && this.sourceCRLF[before - 1] === at - 1;
    return at - before + (inside && association === 1 ? 1 : 0);
  }

  private separatorAt(position: number): string {
    let low = 0, high = this.breaks.length;
    while (low < high) {
      const middle = (low + high) >>> 1;
      if (this.breaks[middle].editor < position) low = middle + 1; else high = middle;
    }
    // Prefer the current line's separator, or the preceding line at EOF.
    return (this.breaks[low] ?? this.breaks[low - 1])?.text ?? '\n';
  }

  // Changes use the original editor document's coordinates, in ascending non-overlapping order.
  apply(changes: readonly SourceChange[]): SourcePositions {
    if (changes.length === 0) return this;
    const parts: string[] = [];
    let cursor = 0;
    for (const change of changes) {
      if (!Number.isInteger(change.from) || !Number.isInteger(change.to) || change.from < cursor
        || change.to < change.from || change.to > this.document.length) throw new RangeError('Invalid source edit range');
      const normalized = change.insert.replace(/\r\n|\r/g, '\n');
      const inserted = normalized === this.document.slice(change.from, change.to)
        ? this.source.slice(this.toSource(change.from), this.toSource(change.to))
        : normalized.replace(/\n/g, this.separatorAt(change.from));
      parts.push(this.source.slice(this.toSource(cursor), this.toSource(change.from)), inserted);
      cursor = change.to;
    }
    parts.push(this.source.slice(this.toSource(cursor)));
    const source = parts.join('');
    return source === this.source ? this : new SourcePositions(source);
  }
}
