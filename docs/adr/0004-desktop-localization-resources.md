# ADR 0004: Data-only desktop language resources

Status: Accepted for English/Japanese desktop presentation.

## Decision

Keep translated text in bundled JSON under `apps/desktop/src/locales/`, grouped
by application state and views. TypeScript owns typed formatting functions,
known-status selection, and singular/plural selection, not translated sentences.
Use named `{parameter}` placeholders with single-pass replacement. Interpolated
values remain literal text and are never parsed again or rendered as HTML.

This replaces the initial source-code catalog approach at the operator's request.
JSON uses the existing TypeScript/Vite toolchain and Node's JSON module imports;
YAML would add a parser dependency without a needed capability. No translation
service, resource loader, ICU grammar, or localization framework is required.

The existing settings transaction owns `locale: en|ja`; saved settings alone
control presentation. Retained frontend notices keep message identity and typed
arguments. Backend faults, log bodies, and log-derived notification bodies stay
original after existing redaction and bounds.

## Evidence and checks

A Node 24.18.0 probe imported both JSON resources with import attributes and
formatted a profile name containing `{id}<script>` without interpreting either
its placeholder-like content or markup. Catalog checks compare every resource
key and named parameter between languages; TypeScript checks the view adapters
and their callers. The normal frontend test/build commands include these checks.

Pure formatting and builds do not prove WebView interaction. Actual desktop
acceptance remains separate. A CI-verifiable implementation PR may be opened
before that acceptance, with unexecuted cases left explicit rather than passed.
