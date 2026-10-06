import {useMemo} from 'react';
import {JsonErrorNotice} from './MetadataControls.tsx';
import type {FileDraft} from '../authoring.ts';
import {parseJson, sourceMapFacts} from '../metadata.ts';
import type {ManifestAsset} from '../metadata.ts';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';

// Generated data: facts only. Regenerating it belongs to the build that produced it.
export function SourceMapView({draft, module}: {draft: FileDraft & {text: string}; module: string | null}) {
  const a = messages[useLocale()].ui.authoring;
  const document = useMemo(() => parseJson(draft.text), [draft.text]);
  const facts = document.ok ? sourceMapFacts(document.value) : null;
  return <div id="authoring-source-map" className="metadata-view">
    <p className="authority-note">{a.sourceMapNote}</p>
    {!document.ok && <JsonErrorNotice id="authoring-source-map-invalid" error={document.error}/>}
    {facts && facts.issues.length > 0 && <ul id="authoring-source-map-issues" className="schema-issues">{facts.issues.map((issue, index) =>
      <li key={index} className="inline-warning" data-issue={issue.code}>{a.sourceMapIssue(issue)}</li>)}</ul>}
    <dl id="authoring-source-map-facts" className="run-identity metadata-facts">
      <dt>{a.sourceMapModule}</dt><dd className="mono">{module ?? a.none}</dd>
      {facts && <>
        <dt>{a.sourceMapVersion}</dt><dd className="mono">{JSON.stringify(facts.version ?? null)}</dd>
        <dt>{a.sourceMapFile}</dt><dd className="mono">{facts.file ?? a.none}</dd>
        <dt>{a.sourceMapSources}</dt><dd>{facts.sources.length === 0 ? a.none
          : <ul className="fact-list">{facts.sources.map((source, index) => <li key={index} className="mono">{source}</li>)}</ul>}</dd>
        <dt>{a.sourceMapNames}</dt><dd>{facts.names ?? a.none}</dd>
        <dt>{a.sourceMapMappings}</dt><dd>{facts.mappings === null ? a.none : a.characters(facts.mappings)}</dd>
        <dt>{a.sourceMapEmbedded}</dt><dd>{facts.embedded}</dd>
      </>}
    </dl>
  </div>;
}

// Assets are inventory: their bytes are listed with the manifest's declaration and never edited here.
export function AssetView({draft, asset}: {draft: FileDraft; asset: ManifestAsset | null}) {
  const a = messages[useLocale()].ui.authoring;
  return <div id="authoring-asset" className="metadata-view">
    <p className="authority-note">{a.assetNote}</p>
    <p id="authoring-binary" className="muted">{a.binary(draft.bytes)}</p>
    <dl className="run-identity metadata-facts">
      <dt>{a.assetId}</dt><dd className="mono">{asset?.id ?? a.none}</dd>
      <dt>{a.assetFormat}</dt><dd className="mono">{asset?.format || a.none}</dd>
      <dt>{a.assetSize}</dt><dd>{asset ? a.assetDimensions(JSON.stringify(asset.width ?? null), JSON.stringify(asset.height ?? null)) : a.none}</dd>
    </dl>
  </div>;
}
