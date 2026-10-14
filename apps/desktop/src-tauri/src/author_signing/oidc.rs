//! Google installed-client enrollment with PKCE, state and nonce over bounded HTTPS.
//! Its build-supplied registration key is extractable public-client configuration,
//! not proof of application identity. Codes, verifiers and tokens never enter faults.
use super::{
    Account, MAX_IDENTIFIER_BYTES, MAX_LABEL_BYTES, bounded_text, cancelled_fault, check_cancelled,
};
use mado_runtime_comparison::model::Fault;
use openidconnect::core::{
    CoreAuthPrompt, CoreAuthenticationFlow, CoreClient, CoreErrorResponseType,
    CoreJwsSigningAlgorithm, CoreProviderMetadata, CoreTokenResponse,
};
use openidconnect::url::{Url, form_urlencoded};
use openidconnect::{
    AccessTokenHash, AuthType, AuthorizationCode, ClaimsVerificationError, ClientId, ClientSecret,
    CsrfToken, DiscoveryError, EndpointMaybeSet, EndpointNotSet, EndpointSet, HttpRequest,
    HttpResponse, IssuerUrl, Nonce, OAuth2TokenResponse, PkceCodeChallenge, PkceCodeVerifier,
    RedirectUrl, RequestTokenError, Scope, SignatureVerificationError, StandardErrorResponse,
    SyncHttpClient, TokenResponse,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddrV4, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use std::{fmt, thread};

#[cfg(test)]
mod tests;

/// The application-approved provider registration. Only Google is supported.
pub(super) struct Provider {
    pub(super) issuer: &'static str,
    pub(super) client_id: &'static str,
    client_key: Option<&'static str>,
    algorithm: CoreJwsSigningAlgorithm,
}

/// Fixed Google Desktop registration; see ADR 0009 for the non-confidential key.
pub(super) const GOOGLE: Provider = Provider {
    issuer: "https://accounts.google.com",
    client_id: "11922665654-rtj4ldpcoloiuf02lv30a9i8lvqnhtuj.apps.googleusercontent.com",
    client_key: option_env!("OIDC_CLIENT_KEY_GOOGLE"),
    algorithm: CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256,
};

/// Total enrollment bound, including discovery, JWKS, browser interaction and token exchange.
const INTERACTION: Duration = Duration::from_secs(5 * 60);
const NETWORK_CALL: Duration = Duration::from_secs(10);
const NETWORK_STEP: Duration = Duration::from_secs(5);
const MAX_RESPONSE_BYTES: u64 = 64 * 1024;
const MAX_RESPONSE_HEADER_BYTES: usize = 16 * 1024;
const CALLBACK_READ: Duration = Duration::from_secs(2);
const MAX_REQUEST_HEAD: usize = 8 * 1024;
const MAX_STRAY_REQUESTS: usize = 16;
const POLL: Duration = Duration::from_millis(100);
const MAX_CODE_BYTES: usize = 2048;
const ISSUED_SKEW_SECONDS: i64 = 5 * 60;
const ISSUED_AGE_SECONDS: i64 = 10 * 60;
const FALLBACK_LABEL: &str = "Google account";
const RECEIVED_PAGE: &str = "<!doctype html><meta charset=\"utf-8\"><title>MadoMata</title><p>MadoMata received the sign-in response. Return to MadoMata to see whether enrollment completed; you can close this tab.</p>";
const REFUSED_PAGE: &str = "<!doctype html><meta charset=\"utf-8\"><title>MadoMata</title><p>MadoMata did not accept this sign-in response. Return to MadoMata.</p>";

type OidcClient = CoreClient<
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointMaybeSet,
    EndpointMaybeSet,
>;
type TokenError = RequestTokenError<NetworkError, StandardErrorResponse<CoreErrorResponseType>>;

pub(super) fn login(
    provider: &Provider,
    cancelled: &AtomicBool,
    open_browser: impl FnOnce(&str) -> Result<(), Fault>,
) -> Result<Account, Fault> {
    check_cancelled(cancelled)?;
    if provider.client_key.is_none_or(str::is_empty) {
        return Err(Fault::new(
            "SigningConfiguration",
            "This build lacks the Google Desktop client key; rebuild with OIDC_CLIENT_KEY_GOOGLE",
        ));
    }
    let deadline = Instant::now() + INTERACTION;
    let network = Network::new(cancelled, deadline);
    let issuer = IssuerUrl::new(provider.issuer.to_owned())
        .map_err(|_| provider_fault("The approved issuer URL is invalid"))?;
    let metadata = CoreProviderMetadata::discover(&issuer, &network).map_err(discovery_fault)?;
    check_cancelled(cancelled)?;
    let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
        .map_err(|error| callback_io("bind the loopback listener", &error))?;
    let port = listener
        .local_addr()
        .map_err(|error| callback_io("inspect the loopback listener", &error))?
        .port();
    let redirect = RedirectUrl::new(format!("http://127.0.0.1:{port}"))
        .map_err(|_| provider_fault("The loopback redirect URL is invalid"))?;
    let client = client(provider, metadata).set_redirect_uri(redirect);
    let (url, state, nonce, verifier) = authorization(&client)?;
    check_cancelled(cancelled)?;
    open_browser(url.as_str())?;
    let code = receive(&listener, &state, deadline, cancelled)?;
    drop(listener);
    let response = exchange(&client, code, verifier, &network)?;
    check_cancelled(cancelled)?;
    verify(provider, &client, &response, &nonce)
}

fn client(provider: &Provider, metadata: CoreProviderMetadata) -> OidcClient {
    OidcClient::from_provider_metadata(
        metadata,
        ClientId::new(provider.client_id.to_owned()),
        provider
            .client_key
            .map(|key| ClientSecret::new(key.to_owned())),
    )
    .set_auth_type(AuthType::RequestBody)
}

/// Each attempt uses fresh state, nonce and an S256 PKCE verifier.
fn authorization(client: &OidcClient) -> Result<(Url, CsrfToken, Nonce, PkceCodeVerifier), Fault> {
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let (url, state, nonce) = client
        .authorize_url(
            CoreAuthenticationFlow::AuthorizationCode,
            CsrfToken::new_random,
            Nonce::new_random,
        )
        .add_scope(Scope::new("email".to_owned()))
        .add_prompt(CoreAuthPrompt::SelectAccount)
        .set_pkce_challenge(challenge)
        .url();
    if url.scheme() != "https" {
        return Err(provider_fault(
            "The provider authorization endpoint is not HTTPS",
        ));
    }
    Ok((url, state, nonce, verifier))
}

/// Google requires its Desktop registration key in addition to the PKCE verifier.
fn exchange(
    client: &OidcClient,
    code: AuthorizationCode,
    verifier: PkceCodeVerifier,
    http: &impl SyncHttpClient<Error = NetworkError>,
) -> Result<CoreTokenResponse, Fault> {
    client
        .exchange_code(code)
        .map_err(|_| provider_fault("The provider did not publish a token endpoint"))?
        .set_pkce_verifier(verifier)
        .request(http)
        .map_err(token_fault)
}

/// Accepts one correlated callback on `/`. Requests for other paths are answered and
/// ignored within a small bound; any callback on `/` without this attempt's state fails.
fn receive(
    listener: &TcpListener,
    state: &CsrfToken,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<AuthorizationCode, Fault> {
    listener
        .set_nonblocking(true)
        .map_err(|error| callback_io("configure the loopback listener", &error))?;
    let mut stray = 0;
    loop {
        check_cancelled(cancelled)?;
        let now = Instant::now();
        if now >= deadline {
            return Err(Fault::new(
                "SigningTimeout",
                "Google sign-in did not complete within five minutes",
            ));
        }
        match listener.accept() {
            Ok((stream, _)) => {
                if let Some(code) = callback(stream, state, deadline)? {
                    return Ok(code);
                }
                stray += 1;
                if stray >= MAX_STRAY_REQUESTS {
                    return Err(Fault::new(
                        "SigningCallback",
                        "Too many unrelated requests reached the sign-in listener",
                    ));
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::Interrupted
                        | io::ErrorKind::ConnectionAborted
                ) =>
            {
                thread::sleep(POLL.min(deadline - now));
            }
            Err(error) => return Err(callback_io("accept the sign-in callback", &error)),
        }
    }
}

fn callback(
    mut stream: TcpStream,
    state: &CsrfToken,
    deadline: Instant,
) -> Result<Option<AuthorizationCode>, Fault> {
    let until = (Instant::now() + CALLBACK_READ).min(deadline);
    // Accepted sockets may inherit the listener's non-blocking mode on BSD/macOS.
    if stream.set_nonblocking(false).is_err() {
        return Ok(None);
    }
    let head = match read_head(&mut stream, until) {
        Ok(Some(head)) => head,
        Ok(None) => return Ok(None),
        Err(fault) => {
            respond(&mut stream, "400 Bad Request", REFUSED_PAGE);
            return Err(fault);
        }
    };
    let Some(target) = request_target(&head) else {
        respond(&mut stream, "400 Bad Request", REFUSED_PAGE);
        return Ok(None);
    };
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != "/" {
        respond(&mut stream, "404 Not Found", "");
        return Ok(None);
    }
    let outcome = parse_callback(query, state);
    if outcome.is_ok() {
        respond(&mut stream, "200 OK", RECEIVED_PAGE);
    } else {
        respond(&mut stream, "400 Bad Request", REFUSED_PAGE);
    }
    outcome.map(Some)
}

fn read_head(stream: &mut TcpStream, until: Instant) -> Result<Option<String>, Fault> {
    let mut head = Vec::with_capacity(1024);
    let mut buffer = [0u8; 1024];
    while !head.windows(4).any(|window| window == b"\r\n\r\n") {
        let remaining = until.saturating_duration_since(Instant::now());
        if remaining.is_zero() || stream.set_read_timeout(Some(remaining)).is_err() {
            return Ok(None);
        }
        let Ok(read) = stream.read(&mut buffer) else {
            return Ok(None);
        };
        if read == 0 {
            return Ok(None);
        }
        if head.len() + read > MAX_REQUEST_HEAD {
            return Err(Fault::new(
                "SigningCallback",
                "The sign-in callback request exceeds the header limit",
            ));
        }
        head.extend_from_slice(&buffer[..read]);
    }
    Ok(String::from_utf8(head).ok())
}

fn request_target(head: &str) -> Option<&str> {
    let line = head.split("\r\n").next()?;
    let mut parts = line.split(' ');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some("GET"), Some(target), Some(version), None)
            if target.starts_with('/') && version.starts_with("HTTP/1.") =>
        {
            Some(target)
        }
        _ => None,
    }
}

fn parse_callback(query: &str, expected: &CsrfToken) -> Result<AuthorizationCode, Fault> {
    let foreign = || {
        Fault::new(
            "SigningCallback",
            "An unsolicited or foreign sign-in callback was refused",
        )
    };
    let (mut state, mut code, mut error) = (None, None, None);
    for (name, value) in form_urlencoded::parse(query.as_bytes()) {
        let slot = match &*name {
            "state" => &mut state,
            "code" => &mut code,
            "error" => &mut error,
            _ => continue,
        };
        if slot.replace(value.into_owned()).is_some() {
            return Err(foreign());
        }
    }
    // Compare digests rather than the secret strings themselves.
    let correlated = state.is_some_and(|state: String| {
        Sha256::digest(state.as_bytes()) == Sha256::digest(expected.secret().as_bytes())
    });
    if !correlated {
        return Err(foreign());
    }
    if let Some(error) = error {
        return Err(Fault::new(
            "SigningRefused",
            "Google sign-in was refused or not completed",
        )
        .with_context(json!({"error": protocol_code(&error)})));
    }
    match code {
        Some(code) if bounded_text(&code, MAX_CODE_BYTES) => Ok(AuthorizationCode::new(code)),
        _ => Err(Fault::new(
            "SigningCallback",
            "The sign-in callback did not contain a usable authorization code",
        )),
    }
}

fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    // The page is advisory; enrollment truth is reported by the application.
    let _ = stream.set_write_timeout(Some(CALLBACK_READ));
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.shutdown(Shutdown::Write);
}

/// Validates the ID token for the approved issuer and client: allowed algorithm, JWKS
/// signature, audience, authorized party, expiry, issue time, nonce and access-token hash.
fn verify(
    provider: &Provider,
    client: &OidcClient,
    response: &CoreTokenResponse,
    nonce: &Nonce,
) -> Result<Account, Fault> {
    let id_token = response
        .id_token()
        .ok_or_else(|| claims_fault("missing_id_token"))?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
        .ok_or_else(|| Fault::new("SigningClock", "The system clock is unusable"))?;
    let verifier = client
        .id_token_verifier()
        .set_allowed_algs([provider.algorithm.clone()])
        .set_issue_time_verifier_fn(move |issued| {
            let issued = issued.timestamp();
            if issued > now + ISSUED_SKEW_SECONDS || issued < now - ISSUED_AGE_SECONDS {
                Err("issued outside the accepted window".to_owned())
            } else {
                Ok(())
            }
        });
    let claims = id_token
        .claims(&verifier, nonce)
        .map_err(|error| claims_fault(claims_reason(&error)))?;
    match claims.authorized_party() {
        Some(party) if party.as_str() != provider.client_id => {
            return Err(claims_fault("authorized_party"));
        }
        None if claims.audiences().len() > 1 => return Err(claims_fault("authorized_party")),
        _ => {}
    }
    if let Some(expected) = claims.access_token_hash() {
        let mismatch = || claims_fault("access_token_hash");
        let algorithm = id_token.signing_alg().map_err(|_| mismatch())?;
        let key = id_token.signing_key(&verifier).map_err(|_| mismatch())?;
        let actual = AccessTokenHash::from_token(response.access_token(), algorithm, key)
            .map_err(|_| mismatch())?;
        if actual != *expected {
            return Err(mismatch());
        }
    }
    let subject = claims.subject().as_str();
    if claims.issuer().as_str() != provider.issuer || !bounded_text(subject, MAX_IDENTIFIER_BYTES) {
        return Err(claims_fault("subject"));
    }
    // A display label only; the verified (issuer, subject, client) triple is the association.
    let display_label = claims
        .email()
        .filter(|_| claims.email_verified() == Some(true))
        .map(|email| email.as_str())
        .filter(|email| bounded_text(email, MAX_LABEL_BYTES))
        .unwrap_or(FALLBACK_LABEL);
    Ok(Account {
        issuer: provider.issuer.to_owned(),
        subject: subject.to_owned(),
        client_id: provider.client_id.to_owned(),
        display_label: display_label.to_owned(),
    })
}

/// Bounded HTTPS transport: no proxy or redirects, finite per-call timeouts within the
/// enrollment deadline, bounded response bodies, and no request after cancellation.
struct Network<'a> {
    agent: ureq::Agent,
    cancelled: &'a AtomicBool,
    deadline: Instant,
}

impl<'a> Network<'a> {
    fn new(cancelled: &'a AtomicBool, deadline: Instant) -> Self {
        let agent = ureq::Agent::config_builder()
            .https_only(true)
            .proxy(None)
            .max_redirects(0)
            .http_status_as_error(false)
            .max_response_header_size(MAX_RESPONSE_HEADER_BYTES)
            .timeout_global(Some(NETWORK_CALL))
            .timeout_resolve(Some(NETWORK_STEP))
            .timeout_connect(Some(NETWORK_STEP))
            .user_agent("MadoMata-Author-Signing/1")
            .build()
            .new_agent();
        Self {
            agent,
            cancelled,
            deadline,
        }
    }
}

impl SyncHttpClient for Network<'_> {
    type Error = NetworkError;

    fn call(&self, request: HttpRequest) -> Result<HttpResponse, NetworkError> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(NetworkError::Cancelled);
        }
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(NetworkError::Timeout);
        }
        let request = self
            .agent
            .configure_request(request)
            .timeout_global(Some(remaining.min(NETWORK_CALL)))
            .build();
        let (parts, mut body) = self.agent.run(request)?.into_parts();
        let bytes = body.with_config().limit(MAX_RESPONSE_BYTES).read_to_vec()?;
        Ok(HttpResponse::from_parts(parts, bytes))
    }
}

/// Redirects are never followed: with zero allowed redirects ureq returns the 3xx response,
/// which discovery and token parsing then refuse.
#[derive(Debug)]
enum NetworkError {
    Cancelled,
    Timeout,
    TooLarge,
    Transport,
}

impl NetworkError {
    fn reason(&self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::Timeout => "timeout",
            Self::TooLarge => "response_too_large",
            Self::Transport => "transport",
        }
    }

    fn fault(&self) -> Fault {
        match self {
            Self::Cancelled => cancelled_fault(),
            Self::Timeout => Fault::new(
                "SigningTimeout",
                "A Google sign-in network request exceeded its time limit",
            ),
            _ => Fault::new("SigningNetwork", "A Google sign-in network request failed")
                .with_context(json!({"reason": self.reason()})),
        }
    }
}

impl From<ureq::Error> for NetworkError {
    fn from(error: ureq::Error) -> Self {
        match error {
            ureq::Error::Timeout(_) => Self::Timeout,
            ureq::Error::BodyExceedsLimit(_) | ureq::Error::LargeResponseHeader(..) => {
                Self::TooLarge
            }
            _ => Self::Transport,
        }
    }
}

impl fmt::Display for NetworkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.reason())
    }
}

impl std::error::Error for NetworkError {}

fn discovery_fault(error: DiscoveryError<NetworkError>) -> Fault {
    match error {
        DiscoveryError::Request(error) => error.fault(),
        DiscoveryError::Response(status, _, _) => {
            provider_fault("Google discovery returned an unusable response")
                .with_context(json!({"status": status.as_u16()}))
        }
        DiscoveryError::Validation(_) => {
            provider_fault("Google discovery did not confirm the approved issuer")
        }
        _ => provider_fault("Google discovery metadata could not be read"),
    }
}

/// Report the protocol code only: even an error description may echo credentials.
fn token_fault(error: TokenError) -> Fault {
    match error {
        RequestTokenError::Request(error) => error.fault(),
        RequestTokenError::ServerResponse(response) => {
            let code = response.error().to_string();
            Fault::new(
                "SigningToken",
                "Google refused the authorization code exchange",
            )
            .with_context(json!({"error": protocol_code(&code)}))
        }
        _ => Fault::new(
            "SigningToken",
            "Google returned an unreadable token response",
        ),
    }
}

fn claims_reason(error: &ClaimsVerificationError) -> &'static str {
    match error {
        ClaimsVerificationError::Expired(_) => "expired_or_issue_time",
        ClaimsVerificationError::InvalidAudience(_) => "audience",
        ClaimsVerificationError::InvalidIssuer(_) => "issuer",
        ClaimsVerificationError::InvalidNonce(_) => "nonce",
        ClaimsVerificationError::InvalidSubject(_) => "subject",
        ClaimsVerificationError::SignatureVerification(
            SignatureVerificationError::DisallowedAlg(_)
            | SignatureVerificationError::UnsupportedAlg(_),
        ) => "algorithm",
        ClaimsVerificationError::SignatureVerification(_) => "signature",
        _ => "claims",
    }
}

fn claims_fault(reason: &str) -> Fault {
    Fault::new(
        "SigningToken",
        "The Google ID token failed validation; no account was associated",
    )
    .with_context(json!({"reason": reason}))
}

fn protocol_code(value: &str) -> Option<&str> {
    (!value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_'))
    .then_some(value)
}

fn provider_fault(message: &str) -> Fault {
    Fault::new("SigningProvider", message)
}

fn callback_io(operation: &str, error: &io::Error) -> Fault {
    Fault::new("SigningCallback", format!("could not {operation}")).with_context(json!({
        "operation": operation,
        "kind": format!("{:?}", error.kind()),
    }))
}
