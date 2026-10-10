import {diagnosticLocation, dirtyDrafts, shortRevision, validationCurrent} from '../authoring.ts';
import type {AuthoringSession} from '../authoring.ts';
import type {Fault} from '../types.ts';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';

interface Props {
  session: AuthoringSession;
  recognitionDirty: boolean;
  validating: boolean;
  onNavigate: (fault: Fault) => void;
}

// Retained saved-revision facts only; the shell owns disclosure, cancellation and owner-checked navigation.
export default function AuthoringValidation({session, recognitionDirty, validating, onNavigate}: Props) {
  const a = messages[useLocale()].ui.authoring;
  const validation = session.validation;
  const current = validationCurrent(session);
  const unsaved = dirtyDrafts(session).length + Number(recognitionDirty);
  return <section id="authoring-validation" aria-label={a.savedValidationHeading}>
    {validation && <p id="authoring-validation-state" className={`tag ${!current ? 'stale' : validation.valid ? 'current' : 'unsaved'}`}>
      {validation.valid ? a.valid(shortRevision(validation.revision)) : a.invalid(shortRevision(validation.revision), validation.diagnostics.length)}</p>}
    <div className="panel-body">
      {validating && <p className="muted" role="status">{a.validating}</p>}
      {!validation && !validating && <p className="muted">{a.validationNone}</p>}
      {validation?.valid && <p id="authoring-validation-count" className="muted">{a.validationDiagnosticCount(validation.diagnostics.length)}</p>}
      {validation && !current && <p className="inline-warning">{a.staleValidation(shortRevision(validation.revision))}</p>}
      {unsaved > 0 && <p className="field-help">{a.unsavedNotValidated}</p>}
      {validation && validation.diagnostics.length > 0 && <ol id="authoring-diagnostics" className="diagnostic-list">
        {validation.diagnostics.map((item, index) => {
          const location = diagnosticLocation(item);
          const where = location ? a.location(location.path, location.line, location.column) : null;
          const draft = location ? session.drafts.get(location.path) : undefined;
          return <li key={index}>
            {where && draft && !draft.missing
              ? <button type="button" className="link diagnostic-location" data-path={draft.path} aria-label={a.goTo(where)} onClick={() => onNavigate(item)}>{where}</button>
              : <span className="muted">{where === null ? a.unlocated : a.diagnosticUnavailable(where)}</span>}
            <strong> {item.category}</strong> <span>{item.message}</span></li>;
        })}</ol>}
    </div>
  </section>;
}
