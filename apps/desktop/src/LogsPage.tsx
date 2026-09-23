import {useEffect, useMemo} from 'react';
import {LOG_LEVELS, viewLogs} from './workspace.ts';
import type {LogFilter, LogScope} from './workspace.ts';
import type {LogBatch, LogEntry} from './types.ts';

export type Losses = Omit<LogBatch, 'entries'>;

interface Props {
  eyebrow: string; heading: string; description: string;
  items: LogEntry[]; evicted: number; limit: number;
  scope: LogScope; onScope?: (scope: LogScope) => void;
  filter: LogFilter; onFilter: (filter: LogFilter) => void;
  losses: Losses; sourceDropped: number | null;
  // Event revealed from a notification card; absent from the store means it was evicted or never delivered.
  reveal: number | null;
  originLabel: (workspaceId: string | null) => string;
  showOrigin: boolean;
}

export default function LogsPage(props: Props) {
  const {eyebrow, heading, description, items, evicted, limit, scope, onScope, filter, onFilter, losses, sourceDropped, reveal, originLabel, showOrigin} = props;
  const view = useMemo(() => viewLogs(items, scope, filter), [items, scope, filter]);
  const filtering = filter.text.trim() !== '' || filter.level !== '';
  const revealed = reveal !== null && items.some(entry => entry.sequence === reveal);
  useEffect(() => {
    document.getElementById('logs-page-heading')?.focus({preventScroll: true});
  }, []);
  useEffect(() => {
    if (reveal === null) return;
    const target = document.getElementById(`log-${reveal}`) ?? document.getElementById('logs-page-heading');
    target?.focus({preventScroll: true});
    target?.scrollIntoView({block: 'center'});
  }, [reveal]);
  const lossVisible = losses.gui_dropped > 0 || losses.file_dropped > 0 || losses.file_errors > 0 || evicted > 0 || (sourceDropped ?? 0) > 0;
  return <>
    <div className="page-heading"><div><span className="eyebrow">{eyebrow}</span><h1 id="logs-page-heading" tabIndex={-1}>{heading}</h1><p>{description}</p></div>
      <div className="actions">{onScope && <label className="inline-label" htmlFor="log-scope">Scope
        <select id="log-scope" value={scope.kind} onChange={event => onScope(event.target.value === 'all' ? {kind: 'all'} : {kind: 'application'})}>
          <option value="application">Application events only</option><option value="all">All retained events</option>
        </select></label>}</div>
    </div>
    {reveal !== null && !revealed && <section className="fault" role="alert"><strong>Event #{reveal} is no longer retained</strong>
      <p>It was evicted from the bounded display buffer or never delivered to the GUI queue. Retained operation outcomes stay on the Run page; file logs are unaffected.</p></section>}
    <section className="panel logs-panel" aria-labelledby="logs-heading">
      <h2 id="logs-heading" className="visually-hidden">{heading}</h2>
      <div className="log-toolbar">
        <div className="search"><label htmlFor="log-search" className="visually-hidden">Search code, message, source, or run</label>
          <input id="log-search" type="search" value={filter.text} placeholder="Search code, message, source, or run ID" spellCheck={false}
            onChange={event => onFilter({...filter, text: event.target.value})}/></div>
        <label htmlFor="log-level" className="visually-hidden">Severity</label>
        <select id="log-level" value={filter.level} onChange={event => onFilter({...filter, level: event.target.value})}>
          <option value="">All levels</option>{LOG_LEVELS.map(level => <option key={level} value={level}>{level.toUpperCase()}</option>)}
        </select>
        {filtering && <button type="button" onClick={() => onFilter({text: '', level: ''})}>Clear filters</button>}
      </div>
      {view.shown.length > 0 ? <ol id="log-list" className="log-list">{[...view.shown].reverse().map(entry => {
        const level = entry.level.toLowerCase();
        return <li id={`log-${entry.sequence}`} tabIndex={-1} className={`log-entry level-${level} ${entry.sequence === reveal ? 'revealed' : ''}`} key={entry.sequence} aria-current={entry.sequence === reveal ? 'true' : undefined}>
          <time dateTime={new Date(entry.time_ms).toISOString()}>{new Date(entry.time_ms).toLocaleTimeString()}</time>
          <span className={`level level-${level}`}>{entry.level}</span>
          <span className="log-source">{entry.source}</span>
          <div className="log-event">
            <p><code>{entry.code}</code> {entry.message}</p>
            <div className="log-meta"><span>#{entry.sequence}</span><code>{entry.run ?? 'no run'}</code>{showOrigin && <span>{originLabel(entry.workspace_id)}</span>}</div>
            <details><summary>Diagnostic fields</summary><pre>{JSON.stringify(entry.fields, null, 2)}</pre></details>
          </div>
        </li>;
      })}</ol>
        : <div className="empty">{view.scoped.length === 0
          ? <p>No retained events for this scope. Events may still have been evicted from the display buffer or written only to file logs.</p>
          : <p>No retained events match the current filters. {view.scoped.length} events in this scope are hidden by filtering.</p>}</div>}
      <div className="log-stats">
        <span>{view.shown.length} shown / {view.scoped.length} in scope / {items.length} retained of {limit}</span>
        <span>Newest first · search matches code, message, source, and run only</span>
      </div>
    </section>
    {lossVisible && <dl className="loss-counters" aria-label="Log loss indicators">
      <div><dt>Display evicted</dt><dd>{evicted}</dd></div>
      {sourceDropped !== null && <div><dt>Source / controller dropped · current operation</dt><dd>{sourceDropped}</dd></div>}
      <div><dt>GUI transport dropped</dt><dd>{losses.gui_dropped}</dd></div>
      <div><dt>File queue dropped</dt><dd>{losses.file_dropped}</dd></div>
      <div><dt>File I/O errors</dt><dd>{losses.file_errors}</dd></div>
    </dl>}
    {losses.last_file_error && <p className="inline-warning">Last file error: {losses.last_file_error}</p>}
    <p className="muted">{lossVisible ? 'Loss counters are cumulative and independent of this page.' : 'No display eviction, transport loss, or file-output failure has been recorded.'} Display eviction does not delete file records or change retained results. A filtered or empty page is not a claim that no events occurred.</p>
  </>;
}
