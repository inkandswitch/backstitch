use openidconnect::ClientId;
use serde::Deserialize;
use thiserror::Error;
use url::Url;

pub struct ServerInfo {
    pub url: Url,
    pub auth: AuthConfig,
    pub webviewer: Option<Url>,
}

pub struct OidcAuthConfig {
    pub issuer: Url,
    pub redirect_port: u16,
    pub client_id: ClientId,
}

pub enum AuthConfig {
    Oidc(OidcAuthConfig),
    None,
}

#[derive(Debug, Deserialize)]
struct HandshakeResponse {
    version: String,
    auth: String,
    webviewer: Option<Url>,
    oidc_issuer: Option<Url>,
    oidc_client_id: Option<String>,
    oidc_redirect_port: Option<u16>,
}

#[derive(Error, Debug)]
pub enum HandshakeError {
    #[error(transparent)]
    Reqwest(#[from] reqwest::Error),
    #[error("the handshake response was malformed: {0}")]
    MalformedResponse(String),
}

pub async fn server_handshake(url: &Url) -> Result<ServerInfo, HandshakeError> {
    let http_client = reqwest::ClientBuilder::new()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;

    let response: HandshakeResponse = http_client
        .get(
            url.join("backstitch-info")
                .expect("URL parsing error in handshake"),
        )
        .send()
        .await?
        .error_for_status()?
        .json()
        .await
        .map_err(|e| HandshakeError::MalformedResponse(e.to_string()))?;

    let auth = match response.auth.as_str() {
        "none" => AuthConfig::None,
        "oidc" => AuthConfig::Oidc(OidcAuthConfig {
            client_id: ClientId::new(response.oidc_client_id.ok_or(
                HandshakeError::MalformedResponse("expected oidc_client_id to exist".to_string()),
            )?),
            issuer: response
                .oidc_issuer
                .ok_or(HandshakeError::MalformedResponse(
                    "expected oidc_issuer to exist".to_string(),
                ))?,
            redirect_port: response
                .oidc_redirect_port
                .ok_or(HandshakeError::MalformedResponse(
                    "expected oidc_redirect_port to exist".to_string(),
                ))?,
        }),
        other => {
            return Err(HandshakeError::MalformedResponse(format!(
                "unsupported auth_type {other}"
            )));
        }
    };

    Ok(ServerInfo {
        url: url.clone(),
        auth,
        webviewer: response.webviewer,
    })
}
