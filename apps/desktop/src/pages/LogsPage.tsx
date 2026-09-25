import {useEffect, useMemo} from 'react';
import Select from '../components/Select.tsx';
import {LOG_LEVELS, viewLogs} from '../workspace.ts';
import type {LogFilter, LogScope} from '../workspace.ts';
import type {LogBatch, LogEntry} from '../types.ts';
import {formatTime, messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';

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
  const locale = useLocale();
  const t = messages[locale].ui;
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
      <div className="actions">{onScope && <div className="inline-label"><label htmlFor="log-scope">{t.logs.scope}</label>
        <Select id="log-scope" value={scope.kind} onChange={value => onScope(value === 'all' ? {kind: 'all'} : {kind: 'application'})}
          options={[{value: 'application', label: t.logs.application}, {value: 'all', label: t.logs.all}]}/></div>}</div>
    </div>
    {reveal !== null && !revealed && <section className="fault" role="alert"><strong>{t.logs.missing(reveal)}</strong>
      <p>{t.logs.missingHelp}</p></section>}
    <section className="panel logs-panel" aria-labelledby="logs-heading">
      <h2 id="logs-heading" className="visually-hidden">{heading}</h2>
      <div className="log-toolbar">
        <div className="log-search-field" role="group" aria-label={t.logs.filters}>
          <label htmlFor="log-level" className="visually-hidden">{t.logs.level}</label>
          <Select id="log-level" value={filter.level} onChange={level => onFilter({...filter, level})}
            options={[{value: '', label: t.logs.allLevels}, ...LOG_LEVELS.map(level => ({value: level, label: t.severity(level)}))]}/>
          <label htmlFor="log-search" className="visually-hidden">{t.logs.search}</label>
          <input id="log-search" type="search" value={filter.text} placeholder={t.logs.searchPlaceholder} spellCheck={false}
            onChange={event => onFilter({...filter, text: event.target.value})}/>
        </div>
        {filtering && <button type="button" onClick={() => onFilter({text: '', level: ''})}>{t.logs.clear}</button>}
      </div>
      {view.shown.length > 0 ? <ol id="log-list" className="log-list">{[...view.shown].reverse().map(entry => {
        const level = entry.level.toLowerCase();
        return <li id={`log-${entry.sequence}`} tabIndex={-1} className={`log-entry level-${level} ${entry.sequence === reveal ? 'revealed' : ''}`} key={entry.sequence} aria-current={entry.sequence === reveal ? 'true' : undefined}>
          <time dateTime={new Date(entry.time_ms).toISOString()}>{formatTime(locale, entry.time_ms)}</time>
          <span className={`level level-${level}`}>{t.severity(entry.level)}</span>
          <span className="log-source">{entry.source}</span>
          <div className="log-event">
            <p><code>{entry.code}</code> {entry.message}</p>
            <div className="log-meta"><span>#{entry.sequence}</span><code>{entry.run ?? t.logs.noRun}</code>{showOrigin && <span>{originLabel(entry.workspace_id)}</span>}</div>
            <details><summary>{t.logs.fields}</summary><pre>{JSON.stringify(entry.fields, null, 2)}</pre></details>
          </div>
        </li>;
      })}</ol>
        : <div className="empty">{view.scoped.length === 0
          ? <p>{t.logs.empty}</p>
          : <p>{t.logs.filtered(view.scoped.length)}</p>}</div>}
      <div className="log-stats">
        <span>{t.logs.stats(view.shown.length, view.scoped.length, items.length, limit)}</span>
        <span>{t.logs.order}</span>
      </div>
    </section>
    {lossVisible && <dl className="loss-counters" aria-label={t.logs.losses}>
      <div><dt>{t.logs.evicted}</dt><dd>{evicted}</dd></div>
      {sourceDropped !== null && <div><dt>{t.logs.sourceDropped}</dt><dd>{sourceDropped}</dd></div>}
      <div><dt>{t.logs.guiDropped}</dt><dd>{losses.gui_dropped}</dd></div>
      <div><dt>{t.logs.fileDropped}</dt><dd>{losses.file_dropped}</dd></div>
      <div><dt>{t.logs.fileErrors}</dt><dd>{losses.file_errors}</dd></div>
    </dl>}
    {losses.last_file_error && <p className="inline-warning">{t.logs.fileError(losses.last_file_error)}</p>}
    <p className="muted">{t.logs.lossHelp(lossVisible)}</p>
  </>;
}
