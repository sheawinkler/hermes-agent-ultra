use hermes_auth::{exchange_authorization_code, exchange_refresh_token, OAuth2Endpoints};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn exchange_authorization_code_parses_token_json() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "at-ok",
            "token_type": "Bearer",
            "expires_in": 120,
            "refresh_token": "rt-ok",
            "scope": "openid"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let endpoints = OAuth2Endpoints {
        authorize_url: "https://id.example/authorize".to_string(),
        token_url: format!("{}/oauth/token", server.uri()),
        client_id: "public-client".to_string(),
        redirect_uri: "http://127.0.0.1:9999/cb".to_string(),
        scopes: vec!["openid".to_string()],
    };

    let cred = exchange_authorization_code(
        "acme",
        &endpoints,
        "auth-code-xyz",
        "pkce-verifier-secret",
    )
    .await
    .expect("token exchange");

    assert_eq!(cred.provider, "acme");
    assert_eq!(cred.access_token, "at-ok");
    assert_eq!(cred.refresh_token.as_deref(), Some("rt-ok"));
    assert_eq!(cred.token_type, "Bearer");
    assert!(cred.expires_at.is_some());
}

#[tokio::test]
async fn exchange_refresh_token_keeps_old_refresh_if_omitted() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "at-new",
            "token_type": "Bearer",
            "expires_in": 60
        })))
        .expect(1)
        .mount(&server)
        .await;

    let endpoints = OAuth2Endpoints {
        authorize_url: "https://id.example/authorize".to_string(),
        token_url: format!("{}/oauth/token", server.uri()),
        client_id: "public-client".to_string(),
        redirect_uri: "http://127.0.0.1:9999/cb".to_string(),
        scopes: vec![],
    };

    let cred = exchange_refresh_token("acme", &endpoints, "old-refresh")
        .await
        .expect("refresh");

    assert_eq!(cred.access_token, "at-new");
    assert_eq!(cred.refresh_token.as_deref(), Some("old-refresh"));
}
