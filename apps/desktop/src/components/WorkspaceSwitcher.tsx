import Select from './Select.tsx';

export interface WorkspaceOption {
  id: string;
  label: string;
  path: string;
  status: {kind: 'ready' | 'busy' | 'dirty' | 'attention'; text: string};
  running: boolean;
  attention: boolean;
}

interface Props {
  items: WorkspaceOption[];
  selectedId: string | null;
  onSelect: (id: string) => void;
  onClose: (() => void) | null;
  closeReason: string | null;
}

export default function WorkspaceSwitcher({items, selectedId, onSelect, onClose, closeReason}: Props) {
  const selected = items.find(item => item.id === selectedId);
  let running = 0;
  let errors = 0;
  for (const item of items) {
    if (item.running) running++;
    if (item.attention) errors++;
  }

  return <>
    <div className="workspace-summary" role="group" aria-label="All workspace status">
      <span id="workspace-running-count" className={running ? 'running-count active' : 'running-count'}>
        {running > 0 && <span className="workspace-status status-busy" aria-hidden="true"/>}Running <strong>{running}/{items.length}</strong>
      </span>
      {errors > 0 && <span id="workspace-error-count" className="workspace-error-count" title={`${errors} workspace${errors === 1 ? '' : 's'} need attention`}>Errors <strong>{errors}</strong></span>}
    </div>
    <Select id="workspace-select" className="workspace-picker" value={selectedId ?? ''}
      disabled={items.length === 0} onChange={onSelect} commitOnTab={false}
      placeholder={items.length ? 'Choose workspace' : 'No open workspaces'}
      aria-describedby="workspace-summary" aria-label={`Workspace: ${selected?.label ?? 'Choose workspace'}`}
      title={selected?.path} listLabel="Workspaces"
      heading={<><span>Workspaces</span><span>{items.length} open</span></>}
      options={items.map(item => ({
        value: item.id, label: item.label, title: item.path,
        description: <span className={`workspace-option-status status-${item.status.kind}`}>
          <span className={`workspace-status status-${item.status.kind}`} aria-hidden="true"/>
          <span>{item.status.text}</span>
        </span>,
      }))}
      action={selected && onClose ? {
        id: 'close-workspace',
        label: <><span className="workspace-close-symbol" aria-hidden="true">×</span>Close selected workspace</>,
        ariaLabel: `Close selected workspace: ${selected.label}`,
        title: closeReason ?? `Close ${selected.label}`,
        disabled: closeReason !== null,
        onClick: onClose,
      } : undefined}/>
  </>;
}
