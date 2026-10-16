# ADR 0011: OCR model choices and rule results

- Status: Accepted
- Date: 2026-10-16
- Scope: Pinned SDK metadata, desktop OCR settings and selected-model acquisition

## Context

The desktop repeats profile identities and assumes one detector/recognizer pair.
Its initial integration design also expected SDK-wide Reference/Fast/Balanced
ratings and made a new model contingent on passing every benchmark rule.

The SDK's merged [ADR 0084](https://github.com/pashifika/mado-pilot/blob/db939f0905f0bff1a8185bef97e46d84b3571c80/docs/adr/0084-ocr-model-choices-and-test-results.md)
supersedes that policy. The PP-OCRv6 tiny-detector/small-recognizer campaign
passed execution and lifecycle checks but failed text rules on both measured
CPU targets and a negative-image rule on Windows. Implemented model choices
remain selectable; scoped rule results describe those limitations without
changing technical admission or the legacy default.

## Decision

Consume the pinned SDK's pure `mado-pilot-ocr` descriptors in the existing runtime
and desktop setup boundary. Do not initialize native libraries to read metadata,
copy a second model catalog, or import a sibling checkout. Engine validation,
saved-frame Trial and Script OCR use the same profile identity and configuration.

Keep stable profile IDs as saved values. Use the SDK's explicit default only for
an unconfigured draft; never rank, hide or replace a model because of benchmark
results. Preserve provider, target, vocabulary and exact resource validation.

Display the SDK model name and readable variant under `OCR model` / `OCRモデル`.
Append the applicable target/provider's passed-rule count over all declared rules,
or `Not run` / `未実施` when no applicable measured results exist. This count is
not a performance ranking or an all-rules-passed claim. Details retain every
Passed, Failed, NotRun and Incomplete result, criteria, report and source scope.
Do not borrow another target's results or turn missing evidence into success.
Local file verification and explicit initialization Check remain separate.

Derive complete model resource sets from SDK asset descriptors, including any
required vocabulary sidecar. Preserve shared legacy installations and receipts.
Bind setup adoption to the selected profile and resource set as well as the
existing dialog and draft revision. Only Save persists settings.

## Rejected alternatives

- Reintroduce Reference/Fast/Balanced labels: contradicts the shipped SDK policy
  and loses failed or unexecuted rule outcomes.
- Hide the new model until every rule passes: conflates user choice with test
  quality and would reopen an already resolved SDK decision.
- Add a third desktop allowlist entry: retains the duplicated authority and makes
  every subsequent model another coordinated frontend/backend edit.
- Depend on the full native facade for metadata: needlessly makes ordinary
  settings and controlled builds depend on native OCR initialization tooling.
