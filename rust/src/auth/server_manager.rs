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
    /// Get the subject associated with this user. A subject is any UUID identifying the user, and may be the same as the username.
    fn subject(&self) -> String;
    /// Get the email associated with this user.
    fn email(&self) -> Option<String>;
    /// Whether the user is valid. This might be false if the user needs an authentication refresh.
    fn is_valid(&self) -> bool;
    /// Get the authorized bearer token to include with HTTP requests, if relevant.
    fn bearer_token(&self) -> Option<SecretString>;
    /// Clone this UserInfo into another box.
    fn clone_box(&self) -> Box<dyn UserInfo>;
}

impl Clone for Box<dyn UserInfo> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}

#[async_trait]
pub trait Authenticator: Send + Sync + Debug {
    /// Authenticate the user, hanging if necessary. User interaction MAY be required here. If so, triggers [Self::status_changed].
    /// Returns an Authenticator-specific [UserInfo] struct.
    async fn interactive_authenticate(&self) -> Result<Box<dyn UserInfo>, Box<dyn AuthError>>;

    /// Authenticate the user if possible to do so without interaction.
    /// If not possible, returns None. Otherwise, returns an Authenticator-specific [UserInfo] struct.
    async fn immediate_authenticate(&self)
    -> Result<Option<Box<dyn UserInfo>>, Box<dyn AuthError>>;

    /// Deauthenticate (log-out) the user.
    async fn deauthenticate(&self) -> Result<(), Box<dyn AuthError>>;

    /// Triggered during an interactive authentication, if user interaction is required.
    async fn status_changed(&self) -> AuthStatus;

    /// Get a friendly, user-readable name to identify the authentication provider.
    fn provider(&self) -> String;
}

#[derive(Debug, Clone)]
enum ServerState {
    Handshaking,
    HandshakeFailed,
    AuthNeeded {
        server_info: ServerInfo,
        authenticator: Arc<dyn Authenticator>,
    },
    Ready {
        server_info: ServerInfo,
        authenticator: Arc<dyn Authenticator>,
        user_info: Box<dyn UserInfo>,
    },
}

/// An API-friendly
pub enum ServerStatus {
    None,
    Handshaking,
    HandshakeFailed,
    AuthNeeded { provider: String },
    Ready { user_info: Box<dyn UserInfo> },
}

#[derive(Debug)]
struct Server {
    // held while the server is handshaking with handshake()
    handshake_lock: Arc<Mutex<()>>,
    state: ServerState,
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

/// Provided as status updates from the auth stream, for a given server.
/// These are the immediately waiting auth events. For a server status that doesn't require auth, use [ServerStatus].
#[derive(Debug, Clone)]
pub enum AuthStatus {
    NeedsUserLogin(Url),
    NeedsUserLogout(Url),
    Idle,
}

#[derive(Error, Debug)]
pub enum ServerError {
    #[error("the user canceled the authentication request")]
    UserCancelled,
    #[error(transparent)]
    Handshake(#[from] HandshakeError),
    #[error(transparent)]
    Auth(Box<dyn std::error::Error + Send + Sync>),
    #[error(transparent)]
    Deauth(Box<dyn std::error::Error + Send + Sync>),
}

impl ServerManager {
    pub fn new() -> Self {
        let (tx, _) = watch::channel(AuthStatus::Idle);
        Self {
            auth_token: Default::default(),
            servers: Default::default(),
            status_tx: Arc::new(tx),
        }
    }

    pub async fn server_status(&self, url: &Url) -> ServerStatus {
        let servers = self.servers.lock().await;
        let Some(server) = servers.get(url) else {
            return ServerStatus::None;
        };
        match &server.state {
            ServerState::Handshaking => ServerStatus::Handshaking,
            ServerState::HandshakeFailed => ServerStatus::HandshakeFailed,
            ServerState::AuthNeeded { authenticator, .. } => ServerStatus::AuthNeeded {
                provider: authenticator.provider(),
            },
            ServerState::Ready { user_info, .. } => ServerStatus::Ready {
                user_info: user_info.clone(),
            },
        }
    }

    /// Handshakes with the server, attempting immediate authentication if possible with no required user interaction.
    /// If the server already has a pending handshake, waits until it completes, and returns that result.
    pub async fn handshake(&self, url: &Url) -> Result<ServerInfo, ServerError> {
        // We only allow one handshake per server at a time.
        let _lock = {
            let mut servers = self.servers.lock().await;
            let entry = servers.entry(url.clone());
            let server = entry.or_insert_with(|| Server {
                handshake_lock: Default::default(),
                state: ServerState::Handshaking,
            });

            let lock_clone = server.handshake_lock.clone();

            // If we don't drop servers here, it could deadlock with an ongoing handshake!
            drop(servers);

            // Setup a lock on our server's handshake_lock.
            lock_clone.lock_owned().await
        };

        // We've now acquired the handshake lock, which means nobody else is handshaking.
        // Check for a cached handshake, now.
        {
            let mut servers = self.servers.lock().await;
            let entry = servers.get_mut(url);
            let server =
                entry.expect("We added this earlier; nobody should have been able to remove it.");

            match &server.state {
                // This means we're the ones who started handshaking
                ServerState::Handshaking => {}
                // Try again...
                ServerState::HandshakeFailed => {}
                // We need an interactive auth, but yes the handshake is ready
                ServerState::AuthNeeded {
                    server_info,
                    authenticator: _,
                } => return Ok(server_info.clone()),
                ServerState::Ready {
                    server_info,
                    authenticator: _,
                    user_info: _,
                } => return Ok(server_info.clone()),
            }

            server.state = ServerState::Handshaking;
        }

        let server_info = handshake::server_handshake(url).await;

        let server_info = match server_info {
            Err(e) => {
                tracing::error!("Server {url} handshake failed, {e:?}");
                let mut servers = self.servers.lock().await;
                let server = servers
                    .get_mut(url)
                    .expect("We added this earlier; nobody should have been able to remove it.");
                server.state = ServerState::HandshakeFailed;
                return Err(e.into());
            }
            Ok(info) => info,
        };

        // Setup the server's authenticator.
        let authenticator: Arc<dyn Authenticator> = {
            match &server_info.auth {
                handshake::AuthConfig::Oidc(config) => {
                    Arc::new(OidcAuthenticator::new(config.clone(), server_info.clone()))
                }
                handshake::AuthConfig::None => Arc::new(NoneAuthenticator::new()),
            }
        };
        let user_info = match authenticator.immediate_authenticate().await {
            Ok(info) => info,
            // consume this error, but log it
            Err(e) => {
                tracing::error!("Error during immediate authentication: {e:?}");
                None
            }
        };

        let mut servers = self.servers.lock().await;
        let server = servers
            .get_mut(url)
            .expect("We added this earlier; nobody should have been able to remove it.");
        server.state = match user_info {
            Some(user_info) => ServerState::Ready {
                authenticator,
                server_info: server_info.clone(),
                user_info,
            },
            None => ServerState::AuthNeeded {
                server_info: server_info.clone(),
                authenticator,
            },
        };
        Ok(server_info)
    }

    /// Authenticate with the server, if needed.
    /// If user intervention is required, will hang until user completes the flow.
    /// Subscribe to [status_changed] for updates.
    pub async fn authenticate(
        &self,
        server_info: &ServerInfo,
    ) -> Result<Box<dyn UserInfo>, ServerError> {
        // Lock: Only one interactive authentication allowed at a time!
        let mut token = self.auth_token.write().await;
        if let Some(token) = token.take() {
            token.cancel();
        }

        *token = Some(CancellationToken::new());

        // We want to keep this held as read(), that way, nobody is allowed to begin another authentication
        // until we've dropped this.
        let token = token.downgrade();

        // Attempt to grab the user info from the cache.
        let authenticator = {
            let mut servers = self.servers.lock().await;
            let server = servers
                .get_mut(&server_info.url)
                .expect("The user shouldn't have a ServerInfo unless it exists");
            match &server.state {
                ServerState::Handshaking => panic!(
                    "We cannot authenticate a handshaking server, so the user shouldn't have a ServerInfo."
                ),
                ServerState::HandshakeFailed => {
                    panic!("The handshake failed, so the user shouldn't have a ServerInfo.")
                }

                ServerState::AuthNeeded { authenticator, .. } => authenticator.clone(),

                ServerState::Ready {
                    authenticator,
                    user_info,
                    ..
                } => {
                    if user_info.is_valid() {
                        return Ok(user_info.clone());
                    }
                    let authenticator = authenticator.clone();
                    server.state = ServerState::AuthNeeded {
                        server_info: server_info.clone(),
                        authenticator: authenticator.clone(),
                    };
                    authenticator
                }
            }
        };

        // Otherwise, we gotta do the actual auth.
        let user_info = select! {
            _ = token.as_ref().unwrap().cancelled() => {
                self.status_tx.send_replace(AuthStatus::Idle);
                return Err(ServerError::UserCancelled)
            },
            res = self.authenticate_inner(&authenticator) => res
        }?;

        // If success, cache it!
        let mut servers = self.servers.lock().await;
        let server = servers
            .get_mut(&server_info.url)
            .expect("There had better be a cached server!");
        server.state = ServerState::Ready {
            server_info: server_info.clone(),
            authenticator,
            user_info: user_info.clone(),
        };
        Ok(user_info)
    }

    async fn authenticate_inner(
        &self,
        authenticator: &Arc<dyn Authenticator>,
    ) -> Result<Box<dyn UserInfo>, ServerError> {
        let authenticate = authenticator.interactive_authenticate();
        tokio::pin!(authenticate);

        Ok(loop {
            select! {
                status = authenticator.status_changed() => {
                    self.status_tx.send_replace(status);
                }
                result = &mut authenticate => {
                    self.status_tx.send_replace(AuthStatus::Idle);
                    break result.map_err(|e| ServerError::Auth(e))?;
                }
            }
        })
    }

    // pub async fn deauthenticate(&self, server_info: &ServerInfo) -> Result<(), ServerError> {
    //     // Lock: Only one authentication allowed at a time!
    //     let mut token = self.auth_token.write().await;
    //     if let Some(token) = token.take() {
    //         token.cancel();
    //     }

    //     *token = Some(CancellationToken::new());

    //     // We want to keep this held as read(), that way, nobody is allowed to begin another authentication
    //     // until we've dropped this.
    //     let token = token.downgrade();

    //     // We always try and do the deauth.
    //     select! {
    //         // If we cancel,
    //         _ = token.as_ref().unwrap().cancelled() => {
    //             self.status_tx.send_replace(AuthStatus::Ok);
    //             return Err(ServerError::UserCancelled)
    //         },
    //         res = self.deauthenticate_inner(server_info) => res
    //     }
    // }

    // async fn deauthenticate_inner(&self, server_info: &ServerInfo) -> Result<(), ServerError> {
    //     let authenticator = self.get_or_create_authenticator(server_info).await;
    //     let deauthenticate = authenticator.deauthenticate();
    //     tokio::pin!(deauthenticate);

    //     let mut servers = self.servers.lock().await;
    //     let server = servers
    //         .get_mut(&server_info.url)
    //         .expect("There had better be a cached server!");
    //     server.user_info = None;

    //     loop {
    //         select! {
    //             status = authenticator.status_changed() => {
    //                 self.status_tx.send_replace(status);
    //             }
    //             result = &mut deauthenticate => {
    //                 self.status_tx.send_replace(AuthStatus::Ok);
    //                 result.map_err(|e| ServerError::Deauth(e))?;
    //                 break;
    //             }
    //         }
    //     }

    //     Ok(())
    // }

    /// Call when the user needs to cancel the authentication/deauthentication.
    pub async fn cancel_wait(&self) {
        let token = self.auth_token.read().await;
        if let Some(token) = token.as_ref() {
            token.cancel();
        }
    }

    pub fn subscribe_status(&self) -> watch::Receiver<AuthStatus> {
        self.status_tx.subscribe()
    }
}
