# ADR 0010: Archive-only package consumption

- Status: Accepted
- Date: 2026-10-12
- Scope: Local installed packages; no encryption, entitlement service or account manager

## Context

Import already decodes a bounded signed archive into `PackageDraft`, and execution
uses its validated in-memory Inventory. Expanding the same payload into a managed
source directory is unnecessary for execution and exposes authoring operations
that recipients do not need. A self-contained archive also needs no separate
receipt as a second integrity or publication authority.

## Decision

Store the original captured bytes at
`<data-dir>/pkgs/<key_id>/<package_id>/<artifact_id>.mmpkg`.
`key_id` is the signing public key's SHA-256 fingerprint, not an OIDC subject or
Server-certified author ID. `package_id` is stable, not a display name;
`artifact_id` is the exact archive-byte SHA-256 digest.

Publish one complete file without replacement through the existing owned staging,
cancellation and cleanup path. Do not persist extracted source, source maps,
envelope sidecars or receipts. Reinspection verifies the archive digest, signature,
path namespace and package identity before static validation. Selection and Start
consume the verified Inventory without reopening a source directory. Import alone
never selects a package or starts it. Different bytes claiming the same package
ID and release version remain a conflict across signer namespaces.

Keep saved `installed` references as package/artifact pairs. Resolve them through
a bounded signer-directory lookup; refuse ambiguous locations. Keep profile and
Target storage unchanged. Before replacing an installed release in the same
Workspace/package, require the same verified signer, then reuse existing profile
reconciliation. Preserve profile IDs and compatible values, fill existing supported
defaults, and require Recovery for incompatible values rather than silently
coercing or deleting them. A different signer requires another Workspace. This is
a single-account workflow, not account switching or key-rotation management.

Installed releases are execution-only in the application. Refuse human and OMP
authoring before creating a source session; offer no source view, Edit or Duplicate
for an installed release. Ordinary source-directory workflows remain unchanged.

Leave previous expanded installations and saved references intact, but do not
keep an expanded-content execution fallback. Reimporting the original `.mmpkg`
restores its unchanged identity; do not reconstruct a different transport from
extracted files. A missing archive is a missing-source outcome, not configuration
corruption. Recognize old managed paths only to prevent authoring bypasses.

## Protection boundary

Format 1 remains an **unencrypted** signed ZIP. Archive-only storage and closed
application authoring do not prevent external inspection by the filesystem owner,
and do not enforce usage rights or prove publisher identity.

A future Server/client 2-of-2 decryption design belongs before the existing bounded
verified-payload-to-Inventory boundary, under a separately reviewed format and
key-delivery policy. It must preserve integrity, size bounds, cancellation and
explicit execution admission. Do not add a no-op decryptor, entitlement provider
or placeholder Server call now. Threshold cooperation cannot revoke plaintext
already obtained by an authorized recipient controlling the operating system.

## Rejected alternatives

- Expanded payload plus a retained archive or receipt: redundant storage and
  integrity authorities without an execution requirement.
- Display names or raw OIDC identifiers in paths: mutable names or unverified
  remote identity, instead of locally verifiable keys.
- Signer-namespaced profiles or account/key-rotation management: broader than the
  accepted single-account workflow; retain the existing settings contract.
- Shared embedded encryption keys: no meaningful recipient-access control.
