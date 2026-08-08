use std::{collections::HashMap, fmt::Debug, sync::Arc};

use async_trait::async_trait;
use secrecy::SecretString;
use thiserror::Error;
use tokio::{
    select,
    sync::{Mutex, RwLock, watch},
};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::auth::{
    handshake::{self, HandshakeError, ServerInfo},
    none::NoneAuthenticator,
    oidc::OidcAuthenticator,
};

pub trait AuthError: Send + Sync + std::error::Error + 'static {}

pub trait UserInfo: Send + Sync + Debug {
    /// Get the username associated with this user
    fn username(&self) -> String;
    /// Whether the user is valid. This might be false if the user needs an authentication refresh.
    fn is_valid(&self) -> bool;
    /// Get the authorized bearer token to include with HTTP requests, if relevant.
    fn bearer_token(&self) -> Option<SecretString>;
    fn clone_box(&self) -> Box<dyn UserInfo>;
}

impl Clone for Box<dyn UserInfo> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}

#[async_trait]
pub trait Authenticator: Send + Sync + Debug {
    async fn authenticate(&self) -> Result<Box<dyn UserInfo>, Box<dyn AuthError>>;
    async fn status_changed(&self) -> AuthStatus;
}

#[derive(Debug)]
struct Server {
    server_info: ServerInfo,
    user_info: Option<Box<dyn UserInfo>>,
    authenticator: Option<Arc<dyn Authenticator>>,
}

#[derive(Debug, Clone)]
pub struct ServerManager {
    /// Allows the one-at-a-time authentication to be canceled.
    /// Note that ALL waiting authentications will be canceled when this is called, if there are
    /// multiple queued!
    auth_token: Arc<RwLock<Option<CancellationToken>>>,
    servers: Arc<Mutex<HashMap<Url, Server>>>,
    status_tx: Arc<watch::Sender<AuthStatus>>,
}

#[derive(Debug, Clone)]
pub enum AuthStatus {
    NeedsUserLogin,
    Ok,
}

#[derive(Error, Debug)]
pub enum ServerError {
    #[error("the user canceled the authentication request")]
    UserCancelled,
    #[error(transparent)]
    Handshake(#[from] HandshakeError),
    #[error(transparent)]
    Auth(#[from] Box<dyn std::error::Error + Send + Sync>),
}

impl ServerManager {
    pub fn new() -> Self {
        let (tx, _) = watch::channel(AuthStatus::Ok);
        Self {
            auth_token: Default::default(),
            servers: Default::default(),
            status_tx: Arc::new(tx),
        }
    }

    pub async fn handshake(&self, url: &Url) -> Result<ServerInfo, ServerError> {
        // If we have a cached handshake, just return that.
        {
            let servers = self.servers.lock().await;
            if let Some(server) = servers.get(url) {
                return Ok(server.server_info.clone());
            }
        }

        let info = handshake::server_handshake(url).await?;
        {
            let mut servers = self.servers.lock().await;
            // It's possible someone added this while we were handshaking. If so, don't overwrite it.
            if let Some(server) = servers.get(url) {
                return Ok(server.server_info.clone());
            }
            servers.insert(
                url.clone(),
                Server {
                    server_info: info.clone(),
                    user_info: None,
                    authenticator: None,
                },
            );
        }
        Ok(info)
    }

    /// Authenticate with the server, if needed.
    /// If user intervention is required, will hang until user completes the flow.
    /// Subscribe to [status_changed] for updates.
    pub async fn authenticate(
        &self,
        server_info: &ServerInfo,
    ) -> Result<Box<dyn UserInfo>, ServerError> {
        // Lock: Only one authentication allowed at a time!
        let mut token = self.auth_token.write().await;
        if let Some(token) = token.take() {
            token.cancel();
        }

        *token = Some(CancellationToken::new());

        // We want to keep this held as read(), that way, nobody is allowed to begin another authentication
        // until we've dropped this.
        let token = token.downgrade();

        // Attempt to grab the user info from the cache
        {
            let servers = self.servers.lock().await;
            if let Some(server) = servers.get(&server_info.url) {
                if let Some(user_info) = &server.user_info {
                    if user_info.is_valid() {
                        return Ok(user_info.clone());
                    }
                }
            }
        }

        // Otherwise, we gotta do the actual auth.
        select! {
            _ = token.as_ref().unwrap().cancelled() => {
                self.status_tx.send_replace(AuthStatus::Ok);
                return Err(ServerError::UserCancelled)
            },
            res = self.authenticate_inner(server_info) => res
        }
    }

    async fn authenticate_inner(
        &self,
        server_info: &ServerInfo,
    ) -> Result<Box<dyn UserInfo>, ServerError> {
        // Attempt to grab the authenticator from the cache, or make it if it hasn't been created.
        let authenticator = {
            let mut servers = self.servers.lock().await;
            let server = servers
                .get_mut(&server_info.url)
                .expect("This should never happen; server_infos are never cleared once cached");
            if let Some(authenticator) = &server.authenticator {
                authenticator.clone()
            } else {
                let authenticator: Arc<dyn Authenticator> = match &server_info.auth {
                    handshake::AuthConfig::Oidc(config) => {
                        Arc::new(OidcAuthenticator::new(config.clone(), server_info.clone()))
                    }
                    handshake::AuthConfig::None => Arc::new(NoneAuthenticator::new()),
                };
                server.authenticator = Some(authenticator.clone());
                authenticator
            }
        };

        let authenticate = authenticator.authenticate();
        tokio::pin!(authenticate);

        let user_info = loop {
            select! {
                status = authenticator.status_changed() => {
                    self.status_tx.send_replace(status);
                }
                result = &mut authenticate => {
                    self.status_tx.send_replace(AuthStatus::Ok);
                    break result.map_err(|e| ServerError::Auth(e))?;
                }
            }
        };

        let mut servers = self.servers.lock().await;
        let server = servers
            .get_mut(&server_info.url)
            .expect("There had better be a cached server!");
        server.user_info = Some(user_info.clone());

        Ok(user_info)
    }

    /// Call when the user needs to cancel the authentication.
    pub async fn cancel_authenticate(&self) {
        let token = self.auth_token.read().await;
        if let Some(token) = token.as_ref() {
            token.cancel();
        }
    }

    pub fn subscribe_status(&self) -> watch::Receiver<AuthStatus> {
        self.status_tx.subscribe()
    }
}
