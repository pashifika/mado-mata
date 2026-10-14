//! Exercises the real loopback listener, request shapes and library-backed token validation
//! with locally signed fixture tokens. No browser, account or network is used.
use super::*;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use openidconnect::core::{
    CoreEdDsaPrivateSigningKey, CoreIdToken, CoreIdTokenClaims, CoreJsonWebKeySet,
    CoreResponseType, CoreSubjectIdentifierType,
};
use openidconnect::{
    AccessToken, AuthUrl, EmptyAdditionalProviderMetadata, JsonWebKeyId, JsonWebKeySetUrl,
    PrivateSigningKey, ResponseTypes, TokenUrl,
};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::io::BufRead;
use std::net::SocketAddr;
use std::sync::Arc;
use std::thread::JoinHandle;

const TEST: Provider = Provider {
    issuer: "https://issuer.example",
    client_id: "client.example",
    client_key: Some("fixture-native-client-key"),
    algorithm: CoreJwsSigningAlgorithm::EdDsa,
};
const NONCE: &str = "nonce-under-test";
const ACCESS: &str = "access-under-test";
const STATE: &str = "state-under-test";

fn now() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap()
}

/// RFC 8410 PKCS#8 wrapping of a fixed fixture seed; no key material is stored.
fn key(seed: u8) -> CoreEdDsaPrivateSigningKey {
    let mut der = vec![
        0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04,
        0x20,
    ];
    der.extend_from_slice(&[seed; 32]);
    let label = "PRIVATE KEY";
    let pem = format!(
        "-----BEGIN {label}-----\n{}\n-----END {label}-----\n",
        STANDARD.encode(der)
    );
    CoreEdDsaPrivateSigningKey::from_ed25519_pem(&pem, Some(JsonWebKeyId::new("fixture".into())))
        .unwrap()
}

fn fixture_client(provider: &Provider) -> OidcClient {
    let metadata = CoreProviderMetadata::new(
        IssuerUrl::new(provider.issuer.into()).unwrap(),
        AuthUrl::new("https://issuer.example/authorize".into()).unwrap(),
        JsonWebKeySetUrl::new("https://issuer.example/jwks".into()).unwrap(),
        vec![ResponseTypes::new(vec![CoreResponseType::Code])],
        vec![CoreSubjectIdentifierType::Public],
        vec![CoreJwsSigningAlgorithm::EdDsa],
        EmptyAdditionalProviderMetadata {},
    )
    .set_token_endpoint(Some(
        TokenUrl::new("https://issuer.example/token".into()).unwrap(),
    ))
    .set_jwks(CoreJsonWebKeySet::new(vec![key(1).as_verification_key()]));
    client(provider, metadata)
}

fn claims(changes: &Value) -> CoreIdTokenClaims {
    let now = now();
    let mut claims = json!({
        "iss": TEST.issuer,
        "aud": [TEST.client_id],
        "exp": now + 600,
        "iat": now,
        "sub": "subject-1",
        "nonce": NONCE,
        "email": "author@example.com",
        "email_verified": true,
    });
    for (name, value) in changes.as_object().unwrap() {
        if value.is_null() {
            claims.as_object_mut().unwrap().remove(name);
        } else {
            claims[name] = value.clone();
        }
    }
    serde_json::from_value(claims).unwrap()
}

/// `hashed` is the access token bound by `at_hash`; the response always carries `ACCESS`.
fn response(signer: u8, changes: &Value, hashed: &str) -> CoreTokenResponse {
    let token = CoreIdToken::new(
        claims(changes),
        &key(signer),
        CoreJwsSigningAlgorithm::EdDsa,
        Some(&AccessToken::new(hashed.into())),
        None,
    )
    .unwrap();
    serde_json::from_value(json!({
        "access_token": ACCESS,
        "token_type": "Bearer",
        "expires_in": 3600,
        "id_token": token.to_string(),
    }))
    .unwrap()
}

fn verified(provider: &Provider, response: &CoreTokenResponse) -> Result<Account, Fault> {
    verify(
        provider,
        &fixture_client(provider),
        response,
        &Nonce::new(NONCE.into()),
    )
}

fn refusal(result: Result<Account, Fault>) -> String {
    let Err(fault) = result else {
        panic!("validation must refuse the token");
    };
    assert_eq!(fault.category, "SigningToken");
    let text = serde_json::to_string(&fault).unwrap();
    assert!(!text.contains(ACCESS) && !text.contains("subject-1") && !text.contains("eyJ"));
    fault.context["reason"].as_str().unwrap().to_owned()
}

#[test]
fn validated_tokens_yield_only_the_local_account_association() {
    let account = verified(&TEST, &response(1, &json!({}), ACCESS)).unwrap();
    assert_eq!(
        (
            account.issuer.as_str(),
            account.subject.as_str(),
            account.client_id.as_str(),
            account.display_label.as_str(),
        ),
        (
            TEST.issuer,
            "subject-1",
            TEST.client_id,
            "author@example.com"
        )
    );
    assert!(verified(&TEST, &response(1, &json!({"azp": TEST.client_id}), ACCESS)).is_ok());
    for changes in [
        json!({"email_verified": false}),
        json!({"email_verified": null}),
        json!({"email": null}),
        json!({"email": " padded@example.com"}),
    ] {
        let account = verified(&TEST, &response(1, &changes, ACCESS)).unwrap();
        assert_eq!(account.display_label, FALLBACK_LABEL, "{changes}");
        assert_eq!(account.subject, "subject-1");
    }
}

#[test]
fn invalid_or_foreign_tokens_are_refused_with_a_reason_only() {
    let now = now();
    for (changes, signer, hashed, expected) in [
        (json!({"nonce": "replayed"}), 1, ACCESS, "nonce"),
        (json!({"nonce": null}), 1, ACCESS, "nonce"),
        (json!({"iss": GOOGLE.issuer}), 1, ACCESS, "issuer"),
        (json!({"aud": ["other.client"]}), 1, ACCESS, "audience"),
        (
            json!({"aud": [TEST.client_id, "other.client"], "azp": TEST.client_id}),
            1,
            ACCESS,
            "audience",
        ),
        (
            json!({"azp": "other.client"}),
            1,
            ACCESS,
            "authorized_party",
        ),
        (json!({"exp": now - 1}), 1, ACCESS, "expired_or_issue_time"),
        (
            json!({"iat": now - ISSUED_AGE_SECONDS - 60}),
            1,
            ACCESS,
            "expired_or_issue_time",
        ),
        (
            json!({"iat": now + ISSUED_SKEW_SECONDS + 60}),
            1,
            ACCESS,
            "expired_or_issue_time",
        ),
        (json!({}), 9, ACCESS, "signature"),
        (json!({}), 1, "substituted", "access_token_hash"),
    ] {
        assert_eq!(
            refusal(verified(&TEST, &response(signer, &changes, hashed))),
            expected,
            "{changes}"
        );
    }
    let rs256_only = Provider {
        issuer: TEST.issuer,
        client_id: TEST.client_id,
        client_key: TEST.client_key,
        algorithm: CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256,
    };
    assert_eq!(
        refusal(verified(&rs256_only, &response(1, &json!({}), ACCESS))),
        "algorithm"
    );
    let without_id_token: CoreTokenResponse =
        serde_json::from_value(json!({"access_token": ACCESS, "token_type": "Bearer"})).unwrap();
    assert_eq!(
        refusal(verified(&TEST, &without_id_token)),
        "missing_id_token"
    );
}

#[test]
fn authorization_requests_use_s256_pkce_fresh_state_nonce_and_the_loopback_redirect() {
    let client = fixture_client(&TEST)
        .set_redirect_uri(RedirectUrl::new("http://127.0.0.1:49152".into()).unwrap());
    let (url, state, nonce, verifier) = authorization(&client).unwrap();
    let query: Vec<(String, String)> = url.query_pairs().into_owned().collect();
    let value = |name: &str| {
        let values: Vec<_> = query.iter().filter(|(key, _)| key == name).collect();
        assert_eq!(values.len(), 1, "{name}");
        values[0].1.clone()
    };
    assert_eq!(value("response_type"), "code");
    assert_eq!(value("client_id"), TEST.client_id);
    assert_eq!(value("redirect_uri"), "http://127.0.0.1:49152");
    assert_eq!(value("code_challenge_method"), "S256");
    assert_eq!(value("scope"), "openid email");
    assert_eq!(value("prompt"), "select_account");
    assert_eq!(value("state"), *state.secret());
    assert_eq!(value("nonce"), *nonce.secret());
    let challenge = PkceCodeChallenge::from_code_verifier_sha256(&verifier);
    assert_eq!(value("code_challenge"), challenge.as_str());
    assert!(query.iter().all(|(key, _)| key != "client_secret"));
    let (_, second_state, second_nonce, _) = authorization(&client).unwrap();
    assert_ne!(second_state.secret(), state.secret());
    assert_ne!(second_nonce.secret(), nonce.secret());
}

#[test]
fn token_exchange_sends_the_native_registration_key_and_pkce_without_logging_credentials() {
    let client = fixture_client(&TEST)
        .set_redirect_uri(RedirectUrl::new("http://127.0.0.1:49152".into()).unwrap());
    let (_, _, _, verifier) = authorization(&client).unwrap();
    let expected_verifier = verifier.secret().clone();
    let sent = RefCell::new(None);
    let refusing = |request: HttpRequest| -> Result<HttpResponse, NetworkError> {
        *sent.borrow_mut() = Some(request);
        Ok(openidconnect::http::Response::builder()
            .status(400)
            .header("content-type", "application/json")
            .body(
                br#"{"error":"invalid_grant","error_description":"code-under-test fixture-native-client-key","echo":"code-under-test"}"#
                    .to_vec(),
            )
            .unwrap())
    };
    let fault = exchange(
        &client,
        AuthorizationCode::new("code-under-test".into()),
        verifier,
        &refusing,
    )
    .unwrap_err();
    assert_eq!(fault.category, "SigningToken");
    assert_eq!(fault.context["error"], "invalid_grant");
    assert!(fault.context.get("description").is_none());
    let diagnostic = serde_json::to_string(&fault).unwrap();
    assert!(!diagnostic.contains("code-under-test"));
    assert!(!diagnostic.contains(TEST.client_key.unwrap()));
    let request = sent.take().unwrap();
    assert_eq!(request.uri(), "https://issuer.example/token");
    assert!(request.headers().get("authorization").is_none());
    let body: Vec<(String, String)> = form_urlencoded::parse(request.body())
        .into_owned()
        .collect();
    for (name, value) in [
        ("grant_type", "authorization_code"),
        ("code", "code-under-test"),
        ("code_verifier", expected_verifier.as_str()),
        ("client_id", TEST.client_id),
        ("client_secret", TEST.client_key.unwrap()),
        ("redirect_uri", "http://127.0.0.1:49152"),
    ] {
        assert!(
            body.iter()
                .any(|(key, found)| key == name && found == value),
            "{name}"
        );
    }
}

#[test]
fn cancelled_or_expired_enrollment_sends_no_request_and_opens_no_browser() {
    let cancelled = AtomicBool::new(true);
    let Err(fault) = login(&GOOGLE, &cancelled, |_| panic!("the browser must not open")) else {
        panic!("a cancelled enrollment must not log in");
    };
    assert_eq!(fault.category, "SigningCancelled");
    let request = || {
        openidconnect::http::Request::builder()
            .uri("https://accounts.google.com/.well-known/openid-configuration")
            .body(Vec::new())
            .unwrap()
    };
    let future = Instant::now() + Duration::from_secs(60);
    assert!(matches!(
        Network::new(&cancelled, future).call(request()),
        Err(NetworkError::Cancelled)
    ));
    let open = AtomicBool::new(false);
    assert!(matches!(
        Network::new(&open, Instant::now()).call(request()),
        Err(NetworkError::Timeout)
    ));
}

#[test]
fn an_unconfigured_build_refuses_before_network_browser_or_callback_binding() {
    let provider = Provider {
        client_key: None,
        ..TEST
    };
    let Err(fault) = login(&provider, &AtomicBool::new(false), |_| {
        panic!("no browser before configuration")
    }) else {
        panic!("missing native registration must refuse");
    };
    assert_eq!(fault.category, "SigningConfiguration");
}

fn loopback() -> (TcpListener, SocketAddr) {
    let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    (listener, address)
}

fn waiting(
    listener: TcpListener,
    cancelled: Arc<AtomicBool>,
    within: Duration,
) -> JoinHandle<Result<String, Fault>> {
    let state = CsrfToken::new(STATE.into());
    // Joining this owner observes teardown; a freed port may already have a new owner.
    thread::spawn(move || {
        receive(&listener, &state, Instant::now() + within, &cancelled)
            .map(|code| code.secret().clone())
    })
}

fn send(address: SocketAddr, target: &str) -> String {
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    write!(stream, "GET {target} HTTP/1.1\r\nHost: {address}\r\n\r\n").unwrap();
    read_response(stream)
}

fn read_response(stream: TcpStream) -> String {
    let mut stream = io::BufReader::new(stream);
    let mut response = String::new();
    let mut length = None;
    loop {
        let start = response.len();
        assert_ne!(stream.read_line(&mut response).unwrap(), 0);
        let line = &response[start..];
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length: ") {
            length = Some(value.trim().parse::<u64>().unwrap());
        }
    }
    // A refused oversized request can reset TCP after the complete HTTP response.
    let length = length.expect("callback response needs Content-Length");
    assert_eq!(
        stream.take(length).read_to_string(&mut response).unwrap() as u64,
        length
    );
    response
}

#[test]
fn only_the_correlated_callback_completes_and_its_listener_then_closes() {
    let (listener, address) = loopback();
    let handle = waiting(listener, Arc::default(), Duration::from_secs(60));
    let idle = TcpStream::connect(address).unwrap();
    assert!(send(address, "/favicon.ico").starts_with("HTTP/1.1 404"));
    let page = send(
        address,
        &format!("/?state={STATE}&code=4%2Fcode-under-test&scope=email&authuser=0"),
    );
    assert!(page.starts_with("HTTP/1.1 200"));
    assert!(!page.contains("code-under-test") && !page.contains(STATE));
    assert_eq!(handle.join().unwrap().unwrap(), "4/code-under-test");
    drop(idle);
}

#[test]
fn foreign_duplicate_incomplete_or_refused_callbacks_fail_the_attempt() {
    for (target, category) in [
        ("/?state=foreign&code=x".to_owned(), "SigningCallback"),
        ("/?code=x".to_owned(), "SigningCallback"),
        (
            format!("/?state={STATE}&state={STATE}&code=x"),
            "SigningCallback",
        ),
        (format!("/?state={STATE}&code=x&code=y"), "SigningCallback"),
        (format!("/?state={STATE}"), "SigningCallback"),
        (format!("/?state={STATE}&code=%01"), "SigningCallback"),
        (
            format!("/?state={STATE}&error=access_denied"),
            "SigningRefused",
        ),
        (
            format!("/?state={STATE}&error=%3Cscript%3E"),
            "SigningRefused",
        ),
    ] {
        let (listener, address) = loopback();
        let handle = waiting(listener, Arc::default(), Duration::from_secs(60));
        assert!(
            send(address, &target).starts_with("HTTP/1.1 400"),
            "{target}"
        );
        let fault = handle.join().unwrap().unwrap_err();
        assert_eq!(fault.category, category, "{target}");
        assert!(!serde_json::to_string(&fault).unwrap().contains(STATE));
        if category == "SigningRefused" {
            let expected = if target.ends_with("access_denied") {
                json!("access_denied")
            } else {
                Value::Null
            };
            assert_eq!(fault.context["error"], expected);
        }
    }
}

#[test]
fn cancellation_deadline_and_stray_requests_end_the_wait() {
    let (listener, _address) = loopback();
    let cancelled = Arc::new(AtomicBool::new(false));
    let handle = waiting(listener, cancelled.clone(), Duration::from_secs(60));
    thread::sleep(Duration::from_millis(250));
    let requested = Instant::now();
    cancelled.store(true, Ordering::Release);
    assert_eq!(
        handle.join().unwrap().unwrap_err().category,
        "SigningCancelled"
    );
    assert!(requested.elapsed() < Duration::from_secs(2));

    let (listener, _) = loopback();
    let started = Instant::now();
    let handle = waiting(listener, Arc::default(), Duration::from_millis(300));
    assert_eq!(
        handle.join().unwrap().unwrap_err().category,
        "SigningTimeout"
    );
    assert!(started.elapsed() < Duration::from_secs(2));

    let (listener, address) = loopback();
    let handle = waiting(listener, Arc::default(), Duration::from_secs(60));
    for _ in 0..MAX_STRAY_REQUESTS {
        assert!(send(address, "/probe").starts_with("HTTP/1.1 404"));
    }
    assert_eq!(
        handle.join().unwrap().unwrap_err().category,
        "SigningCallback"
    );
}

#[test]
fn an_oversized_callback_header_reports_refusal_instead_of_waiting_for_another_redirect() {
    let (listener, address) = loopback();
    let handle = waiting(listener, Arc::default(), Duration::from_secs(60));
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    write!(
        stream,
        "GET /?state={STATE}&code=code-under-test HTTP/1.1\r\nHost: {address}\r\nCookie: local={}\r\n\r\n",
        "a".repeat(MAX_REQUEST_HEAD),
    ).unwrap();
    let response = read_response(stream);
    assert!(response.starts_with("HTTP/1.1 400"));
    assert!(!response.contains(STATE) && !response.contains("code-under-test"));
    assert_eq!(
        handle.join().unwrap().unwrap_err().category,
        "SigningCallback"
    );
}
