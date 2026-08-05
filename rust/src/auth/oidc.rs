use async_trait::async_trait;
use futures::{Stream, StreamExt, stream::BoxStream};
use openidconnect::{
    AccessToken, AccessTokenHash, ClaimsVerificationError, ClientId, ConfigurationError, CsrfToken,
    IssuerUrl, LanguageTag, Nonce, OAuth2TokenResponse, PkceCodeChallenge, RedirectUrl,
    RefreshToken, Scope, SignatureVerificationError, SigningError, TokenResponse,
    core::{CoreAuthenticationFlow, CoreClient, CoreProviderMetadata},
    reqwest,
};
use thiserror::Error;
use tokio::sync::watch;
use tokio_stream::wrappers::WatchStream;

use crate::auth::{
    handshake::{OidcAuthConfig, ServerInfo},
    redirect_server::{RedirectServer, RedirectServerError},
    server_manager::{self, AuthError, AuthStatus, Authenticator, UserInfo},
};

#[derive(Error, Debug)]
pub enum OidcAuthError {
    #[error(transparent)]
    Reqwest(#[from] reqwest::Error),
    // OIDC error types are complete evil nonsense, so we can't just do this nicely.
    #[error("discovery failed {0}")]
    Discovery(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error(transparent)]
    Configuration(#[from] ConfigurationError),
    #[error("code exchange failed {0}")]
    CodeExchange(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Redirect(#[from] RedirectServerError),
    #[error("the authentication server did not provide an ID token")]
    NoIdToken,
    #[error("the authentication server did not provide a refresh token")]
    NoRefreshToken,
    #[error(transparent)]
    ClaimsVerification(#[from] ClaimsVerificationError),
    #[error(transparent)]
    SignatureVerification(#[from] SignatureVerificationError),
    #[error(transparent)]
    Signing(#[from] SigningError),
    #[error("the returned access token mismatched the expected access token hash")]
    TokenMismatch,
}

impl AuthError for OidcAuthError {}

#[derive(Clone)]
pub struct OidcUserInfo {
    pub access_token: AccessToken,
    pub refresh_token: RefreshToken,
    pub subject: String,
    pub name: String,
    pub email: String,
}

impl UserInfo for OidcUserInfo {
    fn username(&self) -> String {
        self.name.clone()
    }

    fn is_valid(&self) -> bool {
        // TODO: implement correctly
        true
    }

    fn clone_box(&self) -> Box<dyn UserInfo> {
        Box::new(self.clone())
    }
}

pub struct OidcAuthenticator {
    status_tx: watch::Sender<AuthStatus>,
    config: OidcAuthConfig,
    server_info: ServerInfo,
}

impl OidcAuthenticator {
    pub fn new(config: OidcAuthConfig, server_info: ServerInfo) -> OidcAuthenticator {
        let (tx, _) = watch::channel(AuthStatus::Ok);
        Self {
            config,
            server_info,
            status_tx: tx,
        }
    }
}

#[async_trait]
impl Authenticator for OidcAuthenticator {
    async fn authenticate(&self) -> Result<Box<dyn UserInfo>, Box<dyn AuthError>> {
        self.auth_inner()
            .await
            .map(|info| Box::new(info) as Box<dyn UserInfo>)
            .map_err(|e| Box::new(e) as Box<dyn AuthError>)
    }

    fn subscribe_status(&self) -> BoxStream<'static, AuthStatus> {
        WatchStream::new(self.status_tx.subscribe()).boxed()
    }
}

impl OidcAuthenticator {
    // TODO: cache a refresh token
    async fn auth_inner(&self) -> Result<OidcUserInfo, OidcAuthError> {
        let http_client = reqwest::ClientBuilder::new()
            .redirect(reqwest::redirect::Policy::none())
            .build()?;

        // We'll need a redirect server to receieve the authorization code.
        // This will shut down when we drop the handle.
        let redirect_server = RedirectServer::new(self.config.redirect_port).await?;

        // Use OpenID Connect Discovery to fetch the provider metadata
        let provider_metadata = CoreProviderMetadata::discover_async(
            IssuerUrl::new(self.config.issuer.to_string()).expect(&format!(
                "URL {} didn't parse right????",
                self.config.issuer
            )),
            &http_client,
        )
        .await
        .map_err(|e| OidcAuthError::Discovery(Box::new(e)))?;

        let client = CoreClient::from_provider_metadata(
            provider_metadata,
            ClientId::new("backstitch".to_string()),
            None,
        )
        // Set the URL the user will be redirected to after the authorization process.
        .set_redirect_uri(
            RedirectUrl::new(format!("http://localhost:{}", redirect_server.port()))
                .expect("??? wtf"),
        );

        // Generate a PKCE challenge.
        let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();

        // Generate the full authorization URL.
        let (auth_url, csrf_token, nonce) = client
            .authorize_url(
                CoreAuthenticationFlow::AuthorizationCode,
                CsrfToken::new_random,
                Nonce::new_random,
            )
            // Set the desired scopes.
            .add_scope(Scope::new("read".to_string()))
            .add_scope(Scope::new("write".to_string()))
            .add_scope(Scope::new("offline_access".to_string())) // request refresh token
            // Set the PKCE code challenge.
            .set_pkce_challenge(pkce_challenge)
            .url();

        // User intervention required here!
        self.status_tx.send_replace(AuthStatus::NeedsUserLogin);
        open::that(auth_url.to_string())?;
        let auth_code = redirect_server.wait_for_redirect(csrf_token).await?;
        self.status_tx.send_replace(AuthStatus::Ok);

        // Exchange our authorization code for some real tokens
        let token_response = client
            .exchange_code(auth_code)?
            .set_pkce_verifier(pkce_verifier)
            .request_async(&http_client)
            .await
            .map_err(|e| OidcAuthError::CodeExchange(Box::new(e)))?;

        // Extract the ID token claims after verifying its authenticity and nonce.
        let id_token = token_response
            .id_token()
            .ok_or_else(|| OidcAuthError::NoIdToken)?;
        let id_token_verifier = client.id_token_verifier();
        let claims = id_token.claims(&id_token_verifier, &nonce)?;

        // Verify the access token hash to ensure that the access token hasn't been substituted for
        // another user's.
        if let Some(expected_access_token_hash) = claims.access_token_hash() {
            let actual_access_token_hash = AccessTokenHash::from_token(
                token_response.access_token(),
                id_token.signing_alg()?,
                id_token.signing_key(&id_token_verifier)?,
            )?;
            if actual_access_token_hash != *expected_access_token_hash {
                return Err(OidcAuthError::TokenMismatch);
            }
        }

        println!(
            "User {} with e-mail address {} has authenticated successfully",
            claims.subject().as_str(),
            claims
                .email()
                .map(|email| email.as_str())
                .unwrap_or("<not provided>"),
        );

        Ok(OidcUserInfo {
            subject: claims.subject().to_string(),
            name: claims
                .name()
                // idk if this all is necessary... it's annoying
                .and_then(|name| {
                    name.get(Some(&LanguageTag::new("en-US".to_string())))
                        .or_else(|| name.get(None))
                })
                .map(|n| n.to_string())
                .unwrap_or("<anonymous".to_string()),
            email: claims
                .email()
                .map(|email| email.as_str())
                .unwrap_or("<no email provided>")
                .to_string(),
            access_token: token_response.access_token().clone(),
            refresh_token: token_response
                .refresh_token()
                .ok_or(OidcAuthError::NoRefreshToken)?
                .clone(),
        })
    }
}
