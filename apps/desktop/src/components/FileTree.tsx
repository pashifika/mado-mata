import {useEffect, useState} from 'react';
import {fileDirty} from '../authoring.ts';
import type {FileDraft} from '../authoring.ts';
import {fileTree, folderAncestors} from '../metadata.ts';
import type {TreeFolder, TreeNode} from '../metadata.ts';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';

interface Props {drafts: readonly FileDraft[]; selected: string | null; onSelect: (path: string) => void}

function dirtyInside(folder: TreeFolder): boolean {
  return folder.children.some(child => child.kind === 'file' ? fileDirty(child.draft) : dirtyInside(child));
}

// Scripts and assets by folder. Rows are disclosure buttons in document order rather than an ARIA tree, so every row
// stays an ordinary focusable button. Collapsing is view state only; selecting a file (from a diagnostic or after Add
// or Rename) opens its folders again.
export default function FileTree({drafts, selected, onSelect}: Props) {
  const a = messages[useLocale()].ui.authoring;
  const [collapsed, setCollapsed] = useState<ReadonlySet<string>>(() => new Set());
  useEffect(() => {
    if (selected === null) return;
    const ancestors = folderAncestors(selected);
    setCollapsed(current => ancestors.some(path => current.has(path)) ? new Set([...current].filter(path => !ancestors.includes(path))) : current);
  }, [selected]);
  const nodes = fileTree(drafts);
  if (nodes.length === 0) return <p id="authoring-tree-empty" className="muted">{a.treeEmpty}</p>;

  function render(list: TreeNode[]) {
    return list.map(node => {
      if (node.kind === 'file') {
        const draft = node.draft;
        return <li key={draft.path}>
          <button type="button" className="tree-file" data-path={draft.path} title={draft.path} aria-current={draft.path === selected ? 'true' : undefined}
            onClick={() => onSelect(draft.path)}>
            <span className="tree-path mono">{node.name}</span>
            {(draft.kind === 'asset' || fileDirty(draft) || draft.diskChanged) && <span className="tree-meta">
              {draft.kind === 'asset' && <span className="tag">{a.kind(draft.kind)}</span>}
              {fileDirty(draft) && <span className="tag unsaved">{a.unsaved}</span>}
              {draft.diskChanged && <span className="tag stale">{a.diskChanged}</span>}</span>}
          </button></li>;
      }
      const expanded = !collapsed.has(node.path);
      const group = `authoring-folder-${encodeURIComponent(node.path)}`;
      return <li key={`${node.path}/`}>
        <button type="button" className="tree-folder" data-folder={node.path} title={node.path} aria-expanded={expanded} aria-controls={group}
          onClick={() => setCollapsed(current => {
            const next = new Set(current);
            if (expanded) next.add(node.path); else next.delete(node.path);
            return next;
          })}>
          <span className="tree-twisty" aria-hidden="true">{expanded ? '▾' : '▸'}</span>
          <span className="tree-path mono">{node.name}/</span>
          {!expanded && dirtyInside(node) && <span className="tag unsaved">{a.unsaved}</span>}
        </button>
        <ul id={group} className="file-tree tree-children" hidden={!expanded}>{render(node.children)}</ul>
      </li>;
    });
  }
  return <ul id="authoring-tree" className="file-tree">{render(nodes)}</ul>;
}
