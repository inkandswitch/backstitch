use std::str::FromStr;

use openidconnect::ClientId;
use serde::Deserialize;
use thiserror::Error;
use url::Url;

const MINIMUM_SERVER_VERSION: &str = "2.0.0";

#[derive(Clone, Debug)]
pub struct ServerInfo {
    pub url: Url,
    pub auth: AuthConfig,
    pub webviewer: Option<Url>,
}

#[derive(Clone, Debug)]
pub struct OidcAuthConfig {
    pub issuer: Url,
    pub redirect_port: u16,
    pub client_id: ClientId,
}

#[derive(Clone, Debug)]
pub enum AuthConfig {
    Oidc(OidcAuthConfig),
    None,
}

#[derive(Debug, Deserialize)]
struct HandshakeResponse {
    version: semver::Version,
    minimum_backstitch_version: semver::Version,
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
    #[error(
        "our Backstitch version is too old (v{current_version}). Please update Backstitch to at least v{minimum_version} to connect to this server."
    )]
    UnsupportedClient {
        current_version: String,
        minimum_version: String,
    },
    #[error(
        "the server you're trying to connect to is too old (v{current_version}). Please update your server to at least {minimum_version}"
    )]
    UnsupportedServer {
        current_version: String,
        minimum_version: String,
    },
}

pub async fn server_handshake(url: &Url) -> Result<ServerInfo, HandshakeError> {
    tracing::debug!("Building HTTP client...");
    let http_client = reqwest::ClientBuilder::new()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(15))
        .build()?;

    tracing::debug!("Waiting for handshake response...");
    let response: HandshakeResponse = http_client
        .get(
            url.join("describe")
                .expect("URL parsing error in handshake"),
        )
        .send()
        .await?
        .error_for_status()?
        .json()
        .await
        .map_err(|e| HandshakeError::MalformedResponse(e.to_string()))?;

    tracing::debug!("Response successfully received.");

    if response.version < semver::Version::from_str(MINIMUM_SERVER_VERSION).unwrap() {
        return Err(HandshakeError::UnsupportedServer {
            current_version: response.version.to_string(),
            minimum_version: MINIMUM_SERVER_VERSION.to_string(),
        });
    }

    if response.minimum_backstitch_version
        < semver::Version::from_str(env!("CARGO_PKG_VERSION")).unwrap()
    {
        return Err(HandshakeError::UnsupportedClient {
            current_version: env!("CARGO_PKG_VERSION").to_string(),
            minimum_version: response.minimum_backstitch_version.to_string(),
        });
    }

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
