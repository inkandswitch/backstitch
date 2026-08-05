use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use futures::{Stream, StreamExt, stream::BoxStream};
use thiserror::Error;
use tokio::{
    select,
    sync::{Mutex, RwLock, watch},
};
use tokio_stream::wrappers::WatchStream;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::auth::{
    handshake::{self, HandshakeError, ServerInfo},
    none::NoneAuthenticator,
    oidc::OidcAuthenticator,
};

pub trait AuthError: Send + Sync + std::error::Error + 'static {}

pub trait UserInfo: Send + Sync {
    /// Get the username associated with this user
    fn username(&self) -> String;
    /// Whether the user is valid. This might be false if the user needs an authentication refresh.
    fn is_valid(&self) -> bool;
    fn clone_box(&self) -> Box<dyn UserInfo>;
}

impl Clone for Box<dyn UserInfo> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}

#[async_trait]
pub trait Authenticator: Send + Sync {
    async fn authenticate(&self) -> Result<Box<dyn UserInfo>, Box<dyn AuthError>>;
    fn subscribe_status(&self) -> BoxStream<'static, AuthStatus>;
}

struct Server {
    server_info: ServerInfo,
    user_info: Option<Box<dyn UserInfo>>,
    authenticator: Arc<dyn Authenticator>,
}

pub struct ServerManager {
    /// Allows the one-at-a-time authentication to be canceled.
    /// Note that ALL waiting authentications will be canceled when this is called, if there are
    /// multiple queued!
    auth_token: RwLock<Option<CancellationToken>>,
    servers: Mutex<HashMap<Url, Server>>,
    status_tx: watch::Sender<AuthStatus>,
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
            status_tx: tx,
        }
    }

    /// Handshake and authenticate with the server, if needed.
    /// If user intervention is required, will hang until user completes the flow.
    /// Subscribe to [status_changed] for updates.
    pub async fn authenticate(&self, url: &Url) -> Result<Box<dyn UserInfo>, ServerError> {
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
            if let Some(server) = servers.get(url) {
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
            res = self.authenticate_inner(url) => res
        }
    }

    async fn authenticate_inner(&self, url: &Url) -> Result<Box<dyn UserInfo>, ServerError> {
        let info = handshake::server_handshake(url).await?;
        let authenticator = match &info.auth {
            handshake::AuthConfig::Oidc(config) => {
                Arc::new(OidcAuthenticator::new(config.clone(), info.clone()))
                    as Arc<dyn Authenticator>
            }
            handshake::AuthConfig::None => {
                Arc::new(NoneAuthenticator::new()) as Arc<dyn Authenticator>
            }
        };

        {
            let mut servers = self.servers.lock().await;
            servers.insert(
                url.clone(),
                Server {
                    server_info: info,
                    authenticator: authenticator.clone(),
                    user_info: None,
                },
            );
        }

        let mut status_stream = authenticator.subscribe_status();
        let user_info = loop {
            select! {
                status = status_stream.next() => {
                    if let Some(status) = status {
                        self.status_tx.send_replace(status);
                    }
                    else {
                        panic!("Authenticator status streams are not allowed to exit early.");
                    }
                }
                result = authenticator.authenticate() => {
                    self.status_tx.send_replace(AuthStatus::Ok);
                    break result.map_err(|e| ServerError::Auth(e))?;
                }
            }
        };

        let mut servers = self.servers.lock().await;
        if let Some(server) = servers.get_mut(url) {
            server.user_info = Some(user_info.clone());
        }

        Ok(user_info)
    }

    /// Call when the user needs to cancel the authentication.
    pub async fn cancel_authenticate(&self) {
        let token = self.auth_token.read().await;
        if let Some(token) = token.as_ref() {
            token.cancel();
        }
    }

    pub fn subscribe_status(&self) -> impl Stream<Item = AuthStatus> {
        WatchStream::new(self.status_tx.subscribe())
    }
}
