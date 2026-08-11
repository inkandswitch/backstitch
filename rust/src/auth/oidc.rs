use std::{
    collections::HashSet,
    str::FromStr,
    time::{Duration, SystemTime},
};

use async_trait::async_trait;
use keyring::Entry;
use openidconnect::{
    AccessToken, AccessTokenHash, ClaimsVerificationError, ConfigurationError, CsrfToken,
    IssuerUrl, LanguageTag, Nonce, OAuth2TokenResponse, PkceCodeChallenge,
    ProviderMetadataWithLogout, RedirectUrl, RefreshToken, Scope, SignatureVerificationError,
    SigningError, TokenResponse,
    core::{
        CoreAuthPrompt, CoreAuthenticationFlow, CoreClient, CoreIdToken, CoreProviderMetadata,
        CoreTokenResponse,
    },
    reqwest,
};
use secrecy::{ExposeSecret, SecretString};
use thiserror::Error;
use tokio::sync::watch;
use url::Url;
use wincode::{SchemaRead, SchemaWrite};

use crate::auth::{
    handshake::{OidcAuthConfig, ServerInfo},
    redirect_server::{RedirectServer, RedirectServerError},
    server_manager::{AuthError, AuthStatus, Authenticator, UserInfo},
};

#[derive(Error, Debug)]
pub enum OidcAuthError {
    #[error(transparent)]
    Reqwest(#[from] reqwest::Error),
    #[error("invalid issuer URL: {0}")]
    InvalidIssuer(String),
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
    #[error("the authentication server did not provide an access token expiry")]
    NoAccessTokenExpiry,
    #[error("we didn't have a stored refresh token")]
    NoRefreshToken,
    #[error("the authentication server provided an incompatible token type {0}")]
    TokenTypeNotSupported(String),
    #[error(transparent)]
    ClaimsVerification(#[from] ClaimsVerificationError),
    #[error(transparent)]
    SignatureVerification(#[from] SignatureVerificationError),
    #[error(transparent)]
    Signing(#[from] SigningError),
    #[error("the returned access token mismatched the expected access token hash")]
    TokenMismatch,
}

#[derive(Error, Debug)]
enum PersistError {
    #[error(transparent)]
    Write(#[from] wincode::WriteError),
    #[error(transparent)]
    Read(#[from] wincode::ReadError),
    #[error(transparent)]
    Keyring(#[from] keyring::v1::Error),
}

impl AuthError for OidcAuthError {}

// Store this as needed in-memory
#[derive(Clone, Debug)]
struct StoredOidcSession {
    pub refresh_token: Option<RefreshToken>,
    pub id_token: SecretString, // IdToken is complicated, don't bother storing it
    pub subject: String,
    pub name: String,
    pub email: Option<String>,
}

// Persist this to disk
#[derive(Clone, SchemaRead, SchemaWrite)]
struct UnsafeStoredOidcSession {
    pub refresh_token: Option<String>,
    pub id_token: String,
    pub subject: String,
    pub name: String,
    pub email: Option<String>,
}

impl From<UnsafeStoredOidcSession> for StoredOidcSession {
    fn from(value: UnsafeStoredOidcSession) -> Self {
        Self {
            refresh_token: value.refresh_token.map(|s| RefreshToken::new(s)),
            id_token: SecretString::from(value.id_token),
            subject: value.subject,
            name: value.name,
            email: value.email,
        }
    }
}

impl From<StoredOidcSession> for UnsafeStoredOidcSession {
    fn from(value: StoredOidcSession) -> Self {
        Self {
            refresh_token: value.refresh_token.map(|s| s.into_secret()),
            id_token: value.id_token.expose_secret().to_string(),
            subject: value.subject,
            name: value.name,
            email: value.email,
        }
    }
}

#[derive(Clone, Debug)]
pub struct OidcUserInfo {
    access_token: AccessToken,
    access_token_expiry: SystemTime,
    stored_session: StoredOidcSession,
}

impl UserInfo for OidcUserInfo {
    fn username(&self) -> String {
        self.stored_session.name.clone()
    }

    fn subject(&self) -> String {
        self.stored_session.subject.clone()
    }

    fn email(&self) -> Option<String> {
        self.stored_session.email.clone()
    }

    fn is_valid(&self) -> bool {
        // Pretend we're 20 secs in the future to be safe
        let now = SystemTime::now()
            .checked_add(Duration::from_secs(20))
            .expect("Time issue...");
        return self.access_token_expiry > now;
    }
    fn bearer_token(&self) -> Option<SecretString> {
        Some(SecretString::from(self.access_token.secret().as_str()))
    }

    fn clone_box(&self) -> Box<dyn UserInfo> {
        Box::new(self.clone())
    }
}

#[derive(Debug)]
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
        self.authenticate()
            .await
            .map(|info| Box::new(info) as Box<dyn UserInfo>)
            .map_err(|e| Box::new(e) as Box<dyn AuthError>)
    }

    async fn deauthenticate(&self) -> Result<(), Box<dyn AuthError>> {
        let res = self
            .deauthenticate()
            .await
            .map_err(|e| Box::new(e) as Box<dyn AuthError>);

        // Even if we errored, always clear the session.
        match self.clear_session() {
            Ok(()) => {}
            Err(e) => tracing::error!("error clearing the stored session: {e}"),
        }
        res
    }

    async fn status_changed(&self) -> AuthStatus {
        let mut rx = self.status_tx.subscribe();
        // I don't think we need to handle this expect
        rx.changed().await.expect("Some recv error??");
        rx.borrow_and_update().clone()
    }
}

impl OidcAuthenticator {
    /// We only accept invalid certs if we've got a server at localhost that's looking for an
    /// OIDC issuer server at localhost that is HTTPS. Otherwise, we don't care.
    /// This is to support local dev servers.
    fn should_accept_invalid_certs(&self) -> Result<bool, OidcAuthError> {
        let issuer_url = Url::from_str(&self.config.issuer)
            .map_err(|_| OidcAuthError::InvalidIssuer(self.config.issuer.clone()))?;
        Ok(issuer_url.scheme() == "https"
            && matches!(
                issuer_url.host_str(),
                Some("localhost" | "127.0.0.1" | "::1")
            )
            && matches!(
                self.server_info.url.host_str(),
                Some("localhost" | "127.0.0.1" | "::1")
            ))
    }

    // TODO (oidc): cache a refresh token
    async fn authenticate(&self) -> Result<OidcUserInfo, OidcAuthError> {
        let http_client = reqwest::ClientBuilder::new()
            .redirect(reqwest::redirect::Policy::none())
            .danger_accept_invalid_certs(self.should_accept_invalid_certs()?)
            .build()?;

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
            self.config.client_id.clone(),
            None,
        )
        // Set the URL the user will be redirected to after the authorization process.
        .set_redirect_uri(
            RedirectUrl::new(format!(
                "http://localhost:{}/auth/oidc/callback",
                self.config.redirect_port
            ))
            .expect("Redirect URL didn't parse right"),
        );

        // Try and retrieve a stored session. If we fail, log the error and continue with an interactive login.
        let stored_session = self
            .retrieve_session()
            .inspect_err(|e| {
                match e {
                    // This one's fine; don't need to log it
                    PersistError::Keyring(keyring::v1::Error::NoEntry) => {}
                    e => {
                        tracing::error!("Error loading stored session: {e}");
                    }
                }
            })
            .ok();

        let stored_session = None;

        Ok(if let Some(session) = stored_session {
            match self.refresh_login(&client, &http_client, &session).await {
                Ok(res) => res,
                Err(e) => {
                    tracing::warn!("Couldn't refresh the login: {e}");
                    self.interactive_login(&client, &http_client).await?
                }
            }
        } else {
            self.interactive_login(&client, &http_client).await?
        })
    }

    async fn interactive_login(
        &self,
        client: &CoreClient<
            openidconnect::EndpointSet,
            openidconnect::EndpointNotSet,
            openidconnect::EndpointNotSet,
            openidconnect::EndpointNotSet,
            openidconnect::EndpointMaybeSet,
            openidconnect::EndpointMaybeSet,
        >,
        http_client: &reqwest::Client,
    ) -> Result<OidcUserInfo, OidcAuthError> {
        // We'll need a redirect server to receieve the authorization code.
        // This will shut down when we drop the handle.
        let redirect_server = RedirectServer::new(self.config.redirect_port).await?;

        // Generate a PKCE challenge.
        let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();

        let scopes = vec![
            Scope::new("email".to_string()),
            Scope::new("profile".to_string()),
            Scope::new("offline_access".to_string()),
        ];

        // Generate the full authorization URL.
        let (auth_url, csrf_token, nonce) = {
            let mut url_request = client
                .authorize_url(
                    CoreAuthenticationFlow::AuthorizationCode,
                    CsrfToken::new_random,
                    Nonce::new_random,
                )
                // Set the desired scopes.
                .add_scopes(scopes.clone()) // request refresh token
                .add_prompt(CoreAuthPrompt::Consent)
                .add_extra_param("resource", "https://api-dev.endlessstudios.com")
                // Set the PKCE code challenge.
                .set_pkce_challenge(pkce_challenge);
            if let Some(resource) = &self.config.resource {
                // url_request = url_request.add_extra_param("resource", resource);
            }
            url_request.url()
        };

        // User intervention required here!
        self.status_tx.send_replace(AuthStatus::NeedsUserLogin);
        tracing::info!("Opening URL: {auth_url}");
        open::that(auth_url.to_string())?;
        let auth_code = redirect_server.wait_for_login(csrf_token).await;
        // Always send OK on failure, then exit.
        self.status_tx.send_replace(AuthStatus::Ok);
        let auth_code = auth_code?;

        // Exchange our authorization code for some real tokens
        let token_response = client
            .exchange_code(auth_code)?
            .set_pkce_verifier(pkce_verifier)
            .request_async(http_client)
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

        if let Some(used_scopes) = token_response.scopes() {
            let scopes = scopes.iter().collect::<HashSet<_>>();
            let used_scopes = used_scopes.iter().collect::<HashSet<_>>();
            for x in scopes.difference(&used_scopes) {
                tracing::warn!("Requested scope {}, but was not granted!", x.as_str());
            }
        }

        // This is actually not required by OIDC.
        // If other authentication systems provide different ways to access this field, we could add those...
        // But right now this is OK.
        let expires_at = Self::access_token_expiry(&token_response).await?;

        match token_response.token_type() {
            openidconnect::core::CoreTokenType::Bearer => {}
            t => return Err(OidcAuthError::TokenTypeNotSupported(format!("{t:?}"))),
        }

        let session = StoredOidcSession {
            refresh_token: token_response.refresh_token().cloned(),
            id_token: SecretString::from(id_token.to_string()),
            subject: claims.subject().to_string(),
            name: claims
                .name()
                // idk if this all is necessary... it's annoying
                .and_then(|name| {
                    name.get(Some(&LanguageTag::new("en-US".to_string())))
                        .or_else(|| name.get(None))
                })
                .map(|n| n.to_string())
                .unwrap_or("<anonymous>".to_string()),
            email: claims.email().map(|email| email.to_string()),
        };

        tracing::info!(
            "User {} <{}> ({}) has authenticated successfully",
            &session.name,
            session.email.as_ref().unwrap_or(&"<no email>".to_string()),
            &session.subject,
        );

        match self.store_session(&session) {
            Ok(()) => {}
            Err(e) => tracing::error!("Error persisting session to disk: {e}"),
        }

        Ok(OidcUserInfo {
            stored_session: session,
            access_token: token_response.access_token().clone(),
            access_token_expiry: expires_at,
        })
    }

    async fn refresh_login(
        &self,
        client: &CoreClient<
            openidconnect::EndpointSet,
            openidconnect::EndpointNotSet,
            openidconnect::EndpointNotSet,
            openidconnect::EndpointNotSet,
            openidconnect::EndpointMaybeSet,
            openidconnect::EndpointMaybeSet,
        >,
        http_client: &reqwest::Client,
        session: &StoredOidcSession,
    ) -> Result<OidcUserInfo, OidcAuthError> {
        let token_response = client
            .exchange_refresh_token(
                session
                    .refresh_token
                    .as_ref()
                    .ok_or(OidcAuthError::NoRefreshToken)?,
            )?
            .request_async(http_client)
            .await
            .map_err(|e| OidcAuthError::CodeExchange(Box::new(e)))?;

        match token_response.token_type() {
            openidconnect::core::CoreTokenType::Bearer => {}
            t => return Err(OidcAuthError::TokenTypeNotSupported(format!("{t:?}"))),
        }

        let mut session = session.clone();
        if let Some(refresh_token) = token_response.refresh_token() {
            session.refresh_token = Some(refresh_token.clone());
        }

        match self.store_session(&session) {
            Ok(()) => {}
            Err(e) => tracing::error!("Error persisting session to disk: {e}"),
        }

        tracing::info!(
            "User {} <{}> ({}) has refreshed successfully",
            &session.name,
            session.email.as_ref().unwrap_or(&"<no email>".to_string()),
            &session.subject,
        );

        Ok(OidcUserInfo {
            stored_session: session,
            access_token: token_response.access_token().clone(),
            access_token_expiry: Self::access_token_expiry(&token_response).await?,
        })
    }

    async fn access_token_expiry(
        token_response: &CoreTokenResponse,
    ) -> Result<SystemTime, OidcAuthError> {
        std::time::SystemTime::now()
            .checked_add(
                token_response
                    .expires_in()
                    .ok_or(OidcAuthError::NoAccessTokenExpiry)?,
            )
            .ok_or(OidcAuthError::NoAccessTokenExpiry)
    }

    async fn deauthenticate(&self) -> Result<(), OidcAuthError> {
        let session = match self.retrieve_session() {
            Ok(session) => session,
            Err(PersistError::Keyring(keyring::v1::Error::NoEntry)) => {
                return Ok(());
            }
            Err(e) => {
                tracing::error!("error retrieving stored session during logout: {e}");
                return Ok(());
            }
        };

        let http_client = reqwest::ClientBuilder::new()
            .redirect(reqwest::redirect::Policy::none())
            .danger_accept_invalid_certs(self.should_accept_invalid_certs()?)
            .build()?;

        let provider_metadata = ProviderMetadataWithLogout::discover_async(
            IssuerUrl::new(self.config.issuer.clone())
                .map_err(|e| OidcAuthError::InvalidIssuer(e.to_string()))?,
            &http_client,
        )
        .await
        .map_err(|e| OidcAuthError::Discovery(Box::new(e)))?;

        let Some(logout_url) = &provider_metadata.additional_metadata().end_session_endpoint else {
            // The provider doesn't advertise RP-initiated logout.
            return Ok(());
        };

        let csrf_token = CsrfToken::new_random();

        let post_logout_redirect_uri = format!(
            "http://localhost:{}/auth/oidc/logged_out",
            self.config.redirect_port
        );

        let mut logout_url = logout_url.url().clone();
        logout_url
            .query_pairs_mut()
            // this is... probably fine to expose?
            .append_pair("id_token_hint", &session.id_token.expose_secret())
            .append_pair("post_logout_redirect_uri", &post_logout_redirect_uri)
            .append_pair("client_id", &self.config.client_id)
            .append_pair("state", csrf_token.secret());

        self.status_tx.send_replace(AuthStatus::NeedsUserLogout);

        let redirect_server = RedirectServer::new(self.config.redirect_port).await?;
        open::that(logout_url.to_string())?;

        // Wait for the OP to complete logout and redirect us back.
        let res = redirect_server
            .wait_for_logout(csrf_token)
            .await
            .map_err(OidcAuthError::Redirect);

        self.status_tx.send_replace(AuthStatus::Ok);
        res?;

        Ok(())
    }

    fn keyring_username(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update("backstitch/auth/oidc".as_bytes());
        hasher.update(self.server_info.url.as_str().as_bytes());
        hasher.update(self.config.client_id.as_bytes());
        hasher.update(self.config.issuer.as_bytes());
        hasher.finalize().to_string()
    }

    fn store_session(&self, session: &StoredOidcSession) -> Result<(), PersistError> {
        let entry = Entry::new("backstitch", &self.keyring_username())?;
        entry.set_secret(&wincode::serialize(&UnsafeStoredOidcSession::from(
            session.clone(),
        ))?)?;
        Ok(())
    }

    fn retrieve_session(&self) -> Result<StoredOidcSession, PersistError> {
        let entry = Entry::new("backstitch", &self.keyring_username())?;
        let secret = entry.get_secret()?;
        let stored: UnsafeStoredOidcSession = wincode::deserialize(&secret)?;
        Ok(StoredOidcSession::from(stored))
    }

    fn clear_session(&self) -> Result<(), PersistError> {
        let entry = Entry::new("backstitch", &self.keyring_username())?;
        entry.delete_credential()?;
        Ok(())
    }
}
