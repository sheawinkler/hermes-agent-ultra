use base64::Engine;
use hermes_auth::{
    exchange_authorization_code, exchange_refresh_token, OAuth2ClientAuthMethod, OAuth2Endpoints,
};
use wiremock::matchers::{body_string_contains, header, method, path};
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
        client_secret: None,
        client_auth_method: OAuth2ClientAuthMethod::default(),
    };

    let cred =
        exchange_authorization_code("acme", &endpoints, "auth-code-xyz", "pkce-verifier-secret")
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
        client_secret: None,
        client_auth_method: OAuth2ClientAuthMethod::default(),
    };

    let cred = exchange_refresh_token("acme", &endpoints, "old-refresh")
        .await
        .expect("refresh");

    assert_eq!(cred.access_token, "at-new");
    assert_eq!(cred.refresh_token.as_deref(), Some("old-refresh"));
}

#[tokio::test]
async fn exchange_authorization_code_supports_confidential_client_basic_auth() {
    let server = MockServer::start().await;
    let basic = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode("client%3Awith+space:secret%3A%40%2F")
    );
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(header("Authorization", basic))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("code_verifier=pkce-verifier-secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "at-confidential",
            "token_type": "Bearer",
            "expires_in": 120,
            "refresh_token": "rt-confidential"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let endpoints = OAuth2Endpoints {
        authorize_url: "https://id.example/authorize".to_string(),
        token_url: format!("{}/oauth/token", server.uri()),
        client_id: "client:with space".to_string(),
        redirect_uri: "http://127.0.0.1:9999/cb".to_string(),
        scopes: vec!["openid".to_string()],
        client_secret: Some("secret:@/".to_string()),
        client_auth_method: OAuth2ClientAuthMethod::ClientSecretBasic,
    };

    let cred =
        exchange_authorization_code("acme", &endpoints, "auth-code-xyz", "pkce-verifier-secret")
            .await
            .expect("token exchange");

    assert_eq!(cred.access_token, "at-confidential");
    assert_eq!(cred.refresh_token.as_deref(), Some("rt-confidential"));
}

#[tokio::test]
async fn exchange_refresh_token_supports_confidential_client_secret_post() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=old-refresh"))
        .and(body_string_contains("client_secret=post-secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "at-post",
            "token_type": "Bearer",
            "expires_in": 60,
            "refresh_token": "rt-post"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let endpoints = OAuth2Endpoints {
        authorize_url: "https://id.example/authorize".to_string(),
        token_url: format!("{}/oauth/token", server.uri()),
        client_id: "post-client".to_string(),
        redirect_uri: "http://127.0.0.1:9999/cb".to_string(),
        scopes: vec![],
        client_secret: Some("post-secret".to_string()),
        client_auth_method: OAuth2ClientAuthMethod::ClientSecretPost,
    };

    let cred = exchange_refresh_token("acme", &endpoints, "old-refresh")
        .await
        .expect("refresh");

    assert_eq!(cred.access_token, "at-post");
    assert_eq!(cred.refresh_token.as_deref(), Some("rt-post"));
}
