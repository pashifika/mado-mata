import {FaultMessage} from './ResultPanel.tsx';
import {shortRevision} from '../authoring.ts';
import {publicationOutcome} from '../distribution.ts';
import type {DistributionReview, Publication} from '../distribution.ts';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';

// The facts of one host review. Package-supplied text renders as text only; key-relative signature validity and the
// unverified account attribution stay separate claims, and source inclusion is disclosed without a secrecy claim.
export function ReviewFacts({review, idPrefix, installing}: {review: DistributionReview; idPrefix: string; installing: boolean}) {
  const locale = useLocale();
  const d = messages[locale].ui.distribution;
  const names = Object.entries(review.game_names);
  return <>
    <dl id={`${idPrefix}-facts`} className="run-identity">
      <dt>{d.name}</dt><dd>{review.name}</dd>
      <dt>{d.packageId}</dt><dd className="mono">{review.package_id}</dd>
      <dt>{d.release}</dt><dd className="mono">{review.release_version}</dd>
      {review.description !== '' && <><dt>{d.description}</dt><dd><p className="distribution-description">{review.description}</p></dd></>}
      {names.length > 0 && <><dt>{d.gameNames}</dt><dd><ul className="distribution-names">
        {names.map(([key, name]) => <li key={key}><span className="mono">{key}</span> · {name}</li>)}</ul></dd></>}
      <dt>{d.revision}</dt><dd className="mono" title={review.revision}>{shortRevision(review.revision)}</dd>
      <dt>{d.content}</dt><dd>{d.contentSummary(review.files, review.bytes)}</dd>
      {installing ? <><dt>{d.compatibility}</dt><dd>{d.compatible(review.runtime)}</dd></> : <><dt>{d.runtime}</dt><dd>{review.runtime}</dd></>}
      <dt>{d.signature}</dt><dd id={`${idPrefix}-signature`}>{d.signatureValid}</dd>
      <dt>{d.keyFingerprint}</dt><dd id={`${idPrefix}-key`} className="mono">{review.key_id}</dd>
      <dt>{d.account}</dt><dd id={`${idPrefix}-account`}>{d.accountUnverified}</dd>
      <dt>{d.artifact}</dt><dd id={`${idPrefix}-artifact`} className="mono">{review.artifact_id}</dd>
      <dt>{installing ? d.installDestination : d.destination}</dt><dd id={`${idPrefix}-destination`} className="mono">{review.destination}</dd>
    </dl>
    <p id={`${idPrefix}-source-included`} className="inline-warning">{d.sourceIncluded}</p>
  </>;
}

// A receipt is shown as success only for the reviewed artifact. A committed artifact whose owned cleanup failed is
// reported as durable with its cleanup fault, never as clean success or as a rollback.
export function PublicationResult({review, publication, idPrefix, installing}: {review: DistributionReview; publication: Publication; idPrefix: string; installing: boolean}) {
  const locale = useLocale();
  const d = messages[locale].ui.distribution;
  const outcome = publicationOutcome(review, publication);
  return <div id={`${idPrefix}-result`} data-outcome={outcome}>
    {outcome === 'clean' && <div className="receipt" role="status"><strong>{installing ? d.installedDone(review.name, review.release_version) : d.exported(publication.path)}</strong>
      {installing && <span className="mono">{publication.path}</span>}</div>}
    {outcome === 'unchanged' && <div className="receipt" role="status"><strong>{d.unchanged}</strong><span className="mono">{publication.path}</span></div>}
    {outcome === 'cleanup' && <>
      <p className="inline-warning" role="alert">{d.cleanupFailed(publication.path)}</p>
      <FaultMessage title={d.cleanupFault} value={publication.cleanup_error!}/>
    </>}
    {outcome === 'uncommitted' && <p className="inline-warning" role="alert">{d.uncommitted}</p>}
    {outcome === 'mismatch' && <p className="inline-warning" role="alert">{d.mismatch} <span className="mono">{publication.path}</span></p>}
  </div>;
}
