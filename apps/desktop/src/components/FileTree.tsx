import {useState} from 'react';
import {fileDirty} from '../authoring.ts';
import type {FileDraft} from '../authoring.ts';
import {elementAnchor, menuEvents} from './ContextMenu.tsx';
import type {MenuAnchor} from './ContextMenu.tsx';
import {fileTree, folderAncestors} from '../metadata.ts';
import type {TreeFolder, TreeNode} from '../metadata.ts';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';

interface Props {
  drafts: readonly FileDraft[]; selected: string | null; onSelect: (path: string) => void;
  // Changes when the page reveals the selected file (a diagnostic), so its folders open even when it was already selected.
  reveal: number;
  // `trigger` is true for the row's visible menu button, which toggles the menu instead of reopening it.
  onMenu: (path: string, anchor: MenuAnchor, opener: HTMLElement, trigger: boolean) => void;
  // `file:<path>` of the row whose menu is open.
  menuOpen: string | null;
}

function inside(folder: TreeFolder, test: (draft: FileDraft) => boolean): boolean {
  return folder.children.some(child => child.kind === 'file' ? test(child.draft) : inside(child, test));
}

// Scripts and assets by folder. Files expose Rename/Remove; folders are disclosure controls only.
// Collapsing is view state; selecting or revealing a file opens its ancestors before the page scrolls to it.
export default function FileTree({drafts, selected, reveal, onSelect, onMenu, menuOpen}: Props) {
  const a = messages[useLocale()].ui.authoring;
  const [collapsed, setCollapsed] = useState<ReadonlySet<string>>(() => new Set());
  const shownKey = `${reveal}:${selected ?? ''}`;
  const [shown, setShown] = useState(shownKey);
  if (shown !== shownKey) {
    setShown(shownKey);
    const ancestors = selected === null ? [] : folderAncestors(selected);
    if (ancestors.some(path => collapsed.has(path))) setCollapsed(new Set([...collapsed].filter(path => !ancestors.includes(path))));
  }
  const nodes = fileTree(drafts);
  if (nodes.length === 0) return <p id="authoring-tree-empty" className="muted">{a.treeEmpty}</p>;

  function trigger(path: string) {
    const key = `file:${path}`;
    return <button type="button" className="row-menu" aria-haspopup="menu" aria-expanded={menuOpen === key} aria-label={a.fileActions(path)} title={a.fileActions(path)}
      data-menu={key} onClick={event => onMenu(path, elementAnchor(event.currentTarget, event.detail === 0), event.currentTarget, true)}
      {...menuEvents((anchor, opener) => onMenu(path, anchor, opener, false))}><span aria-hidden="true">⋯</span></button>;
  }

  function render(list: TreeNode[]) {
    return list.map(node => {
      if (node.kind === 'file') {
        const draft = node.draft;
        const current = draft.path === selected;
        return <li key={draft.path}><div className={current ? 'tree-row current' : 'tree-row'}>
          <button type="button" className="tree-file" data-path={draft.path} title={draft.path} aria-current={current ? 'true' : undefined}
            aria-keyshortcuts="Shift+F10" onClick={() => onSelect(draft.path)} {...menuEvents((anchor, opener) => onMenu(draft.path, anchor, opener, false))}>
            <span className="tree-name mono">{node.name}</span>
            {(draft.kind === 'asset' || fileDirty(draft) || draft.diskChanged) && <span className="tree-meta">
              {draft.kind === 'asset' && <span className="tag">{a.kind(draft.kind)}</span>}
              {fileDirty(draft) && <span className="tag unsaved">{a.unsaved}</span>}
              {draft.diskChanged && <span className="tag stale">{a.diskChanged}</span>}</span>}
          </button>
          {trigger(draft.path)}
        </div></li>;
      }
      const expanded = !collapsed.has(node.path);
      const group = `authoring-folder-${encodeURIComponent(node.path)}`;
      // A collapsed folder still reports unsaved and changed-on-disk files inside it.
      const unsaved = !expanded && inside(node, fileDirty);
      const stale = !expanded && inside(node, draft => draft.diskChanged);
      return <li key={`${node.path}/`}>
        <div className="tree-row">
          <button type="button" className="tree-folder" data-folder={node.path} title={node.path} aria-expanded={expanded} aria-controls={group}
            onClick={() => setCollapsed(current => {
              const next = new Set(current);
              if (expanded) next.add(node.path); else next.delete(node.path);
              return next;
            })}>
            <span className="tree-twisty" aria-hidden="true">{expanded ? '▾' : '▸'}</span>
            <span className="tree-name mono">{node.name}/</span>
            {(unsaved || stale) && <span className="tree-meta">
              {unsaved && <span className="tag unsaved">{a.unsaved}</span>}
              {stale && <span className="tag stale">{a.diskChanged}</span>}</span>}
          </button>
        </div>
        <ul id={group} className="file-tree tree-children" hidden={!expanded}>{render(node.children)}</ul>
      </li>;
    });
  }
  return <ul id="authoring-tree" className="file-tree">{render(nodes)}</ul>;
}
