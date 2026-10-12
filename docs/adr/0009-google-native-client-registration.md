# ADR 0009: Google Desktop client registration

- Status: Accepted
- Date: 2026-10-11
- Scope: Desktop author enrollment; no authentication server or release packaging

## Evidence

An authorized macOS external-browser enrollment reached Google's token endpoint
with the registered Desktop client, loopback callback and PKCE. The exchange
without `client_secret` returned `invalid_request` with
`client_secret is missing.` No signing key was published. This disproved the
initial assumption that this registration could exchange codes with the client
ID and PKCE alone.

[RFC 8252, section 8.5](https://www.rfc-editor.org/rfc/rfc8252#section-8.5)
explicitly addresses native providers that require a shared registration secret:
the application remains a public client, and the embedded value cannot establish
client identity or be considered confidential.

## Decision

Keep the fixed Google Desktop registration and external-browser flow. At Rust
compile time, read `OIDC_CLIENT_KEY_GOOGLE` as the `client_secret` string, not a
JSON document. Send it in the HTTPS token request body alongside the client ID,
code, loopback redirect and PKCE verifier through `openidconnect`.

The registration key is extractable from a configured binary. Supplying it
through a build environment or GitHub Secret keeps it out of source and ordinary
logs; it does **not** make the distributed value confidential. Do not use it as
proof of an authentic MadoMata client, publisher identity or entitlement. Never
substitute a confidential web-client credential.

The application still requires fresh PKCE S256, state and nonce, issuer/JWKS
verification, permitted token algorithms, audience/authorized-party checks and
finite token/interaction lifetimes. Token errors report protocol codes, not
provider descriptions that could echo credentials. The independently random
Ed25519 private key and transient user tokens are separate secrets; none enters
the binary registration setting, frontend responses or package payloads.

Public checks compile and test without this build input or a real account.
An unconfigured build refuses enrollment before network/browser activity with
`SigningConfiguration`; editing, import, controlled execution and signing with
an already enrolled local key remain available. Changing the build input requires
rebuilding and restarting MadoMata, not restarting the editor that launches Cargo.
No runtime environment override or downloaded-package registration is accepted.

For a separately approved trusted build using GitHub environment `release`, map
its `OIDC_CLIENT_KEY_GOOGLE` secret to the same build environment variable.
Do not expose it to pull-request checks, print it, or put it on a command line.
This decision does not create a release workflow or alter repository settings.

## Rejected alternatives

- Continue the secret-free exchange: the real registration refused it.
- Add a token-exchange server: unnecessary infrastructure outside local signing.
- Change providers or add a provider framework: not needed for the selected Google
  registration and would not improve local signing-key custody.
