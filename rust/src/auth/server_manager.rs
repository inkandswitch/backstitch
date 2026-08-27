use std::{collections::HashMap, fmt::Debug, sync::Arc};

use async_trait::async_trait;
use secrecy::SecretString;
use thiserror::Error;
use tokio::{
    select,
    sync::{Mutex, RwLock, broadcast, watch},
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
    async fn interactive_deauthenticate(&self) -> Result<(), Box<dyn AuthError>>;

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

/// The status of a tracked server.
#[derive(Debug, Clone)]
pub enum ServerStatus {
    None,
    Handshaking,
    HandshakeFailed,
    AuthNeeded {
        provider: String,
    },
    Ready {
        provider: String,
        user_info: Box<dyn UserInfo>,
        server_info: ServerInfo,
    },
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
    auth_status_tx: watch::Sender<AuthStatus>,
    // this is a little awkward
    server_status_tx: broadcast::Sender<(Url, ServerStatus)>,
}

/// Provided as status updates from the auth stream during the interactive authentication process, for a given server.
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
        let (auth_tx, _) = watch::channel(AuthStatus::Idle);
        let (server_tx, _) = broadcast::channel(256);
        Self {
            auth_token: Default::default(),
            servers: Default::default(),
            auth_status_tx: auth_tx,
            server_status_tx: server_tx,
        }
    }

    pub async fn server_status(&self, url: &Url) -> ServerStatus {
        let servers = self.servers.lock().await;
        let Some(server) = servers.get(url) else {
            return ServerStatus::None;
        };
        Self::state_to_status(&server.state)
    }

    fn state_to_status(state: &ServerState) -> ServerStatus {
        match state {
            ServerState::Handshaking => ServerStatus::Handshaking,
            ServerState::HandshakeFailed => ServerStatus::HandshakeFailed,
            ServerState::AuthNeeded { authenticator, .. } => ServerStatus::AuthNeeded {
                provider: authenticator.provider(),
            },
            ServerState::Ready {
                user_info,
                server_info,
                authenticator,
            } => ServerStatus::Ready {
                user_info: user_info.clone(),
                server_info: server_info.clone(),
                provider: authenticator.provider(),
            },
        }
    }

    /// If the server returns UNAUTHORIZED, we should invalidate it here.
    /// If we're able to re-validate the UserInfo, returns the new UserInfo.
    /// Otherwise returns None (this will require a full authentication).
    // TODO: This probably has bugs when called by multiple callers (i.e. if we need endpoints other than /sync.)
    // To fix this, we'd want to make UserInfo a handle instead of a raw struct; that way, callers can always get the most recent tok.
    pub async fn try_reauthenticate(&self, server_info: &ServerInfo) -> Option<Box<dyn UserInfo>> {
        let mut servers = self.servers.lock().await;
        let server = servers.get_mut(&server_info.url)?;

        let ServerState::Ready {
            server_info,
            authenticator,
            user_info,
        } = &server.state
        else {
            return None;
        };

        let new_user_info = if user_info.is_valid() {
            None
        } else {
            authenticator
                .immediate_authenticate()
                .await
                .inspect_err(|e| tracing::error!("Error during immediate authenticate: {e}"))
                .ok()
                .flatten()
        };

        // If the user info isn't valid (i.e. the token has expired) we can try reauthenticating
        let state = match new_user_info {
            Some(user_info) => ServerState::Ready {
                server_info: server_info.clone(),
                authenticator: authenticator.clone(),
                user_info: user_info.clone(),
            },
            None => ServerState::AuthNeeded {
                server_info: server_info.clone(),
                authenticator: authenticator.clone(),
            },
        };

        self.set_server_state(server_info.url.clone(), server, state);

        match &server.state {
            ServerState::Ready { user_info, .. } => Some(user_info.clone()),
            _ => None,
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

            self.set_server_state(url.clone(), server, ServerState::Handshaking);
        }

        let server_info = handshake::server_handshake(url).await;

        let server_info = match server_info {
            Err(e) => {
                tracing::error!("Server {url} handshake failed, {e:?}");
                let mut servers = self.servers.lock().await;
                let server = servers
                    .get_mut(url)
                    .expect("We added this earlier; nobody should have been able to remove it.");
                self.set_server_state(url.clone(), server, ServerState::HandshakeFailed);
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
        self.set_server_state(
            url.clone(),
            server,
            match user_info {
                Some(user_info) => ServerState::Ready {
                    authenticator,
                    server_info: server_info.clone(),
                    user_info,
                },
                None => ServerState::AuthNeeded {
                    server_info: server_info.clone(),
                    authenticator,
                },
            },
        );
        Ok(server_info)
    }

    fn set_server_state(&self, url: Url, server: &mut Server, state: ServerState) {
        server.state = state;
        let _ = self
            .server_status_tx
            .send((url, Self::state_to_status(&server.state)));
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
                    self.set_server_state(
                        server_info.url.clone(),
                        server,
                        ServerState::AuthNeeded {
                            server_info: server_info.clone(),
                            authenticator: authenticator.clone(),
                        },
                    );
                    authenticator
                }
            }
        };

        // Otherwise, we gotta do the actual auth.
        let user_info = select! {
            _ = token.as_ref().unwrap().cancelled() => {
                self.auth_status_tx.send_replace(AuthStatus::Idle);
                return Err(ServerError::UserCancelled)
            },
            res = self.authenticate_inner(&authenticator) => res
        }?;

        // If success, cache it!
        let mut servers = self.servers.lock().await;
        let server = servers
            .get_mut(&server_info.url)
            .expect("There had better be a cached server!");
        self.set_server_state(
            server_info.url.clone(),
            server,
            ServerState::Ready {
                server_info: server_info.clone(),
                authenticator,
                user_info: user_info.clone(),
            },
        );
        Ok(user_info)
    }

    /// Returns the cached user info if the server is already authenticated.
    /// This never performs authentication or reauthentication. If the server is
    /// not currently authenticated with valid cached user info, returns `None`.
    pub async fn try_authenticate(&self, server_info: &ServerInfo) -> Option<Box<dyn UserInfo>> {
        let servers = self.servers.lock().await;
        let server = servers.get(&server_info.url)?;

        match &server.state {
            ServerState::Ready { user_info, .. } if user_info.is_valid() => Some(user_info.clone()),
            _ => None,
        }
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
                    self.auth_status_tx.send_replace(status);
                }
                result = &mut authenticate => {
                    self.auth_status_tx.send_replace(AuthStatus::Idle);
                    break result.map_err(|e| ServerError::Auth(e))?;
                }
            }
        })
    }

    /// Deuthenticate with the server (logout), if possible.
    /// If user intervention is required, will hang until user completes the flow.
    /// Subscribe to [status_changed] for updates.
    pub async fn deauthenticate(&self, server_info: &ServerInfo) -> Result<(), ServerError> {
        // don't deauthenticate a non-authenticated server
        match server_info.auth {
            handshake::AuthConfig::Oidc(_) => {}
            handshake::AuthConfig::None => return Ok(()),
        }

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
                    "We cannot deauthenticate a handshaking server, so the user shouldn't have a ServerInfo."
                ),
                ServerState::HandshakeFailed => {
                    panic!("The handshake failed, so the user shouldn't have a ServerInfo.")
                }
                ServerState::AuthNeeded { authenticator, .. } => authenticator.clone(),
                ServerState::Ready { authenticator, .. } => authenticator.clone(),
            }
        };

        let res = select! {
            _ = token.as_ref().unwrap().cancelled() => {
                self.auth_status_tx.send_replace(AuthStatus::Idle);
                return Err(ServerError::UserCancelled)
            },
            res = self.deauthenticate_inner(&authenticator) => res
        };

        // Set back to AuthNeeded even on failure
        let mut servers = self.servers.lock().await;
        let server = servers
            .get_mut(&server_info.url)
            .expect("There had better be a cached server!");

        self.set_server_state(
            server_info.url.clone(),
            server,
            ServerState::AuthNeeded {
                server_info: server_info.clone(),
                authenticator,
            },
        );
        res
    }

    async fn deauthenticate_inner(
        &self,
        authenticator: &Arc<dyn Authenticator>,
    ) -> Result<(), ServerError> {
        let deauthenticate = authenticator.interactive_deauthenticate();
        tokio::pin!(deauthenticate);

        Ok(loop {
            select! {
                status = authenticator.status_changed() => {
                    self.auth_status_tx.send_replace(status);
                }
                result = &mut deauthenticate => {
                    self.auth_status_tx.send_replace(AuthStatus::Idle);
                    break result.map_err(|e| ServerError::Deauth(e))?;
                }
            }
        })
    }

    /// Call when the user needs to cancel the authentication/deauthentication.
    pub async fn cancel_wait(&self) {
        let token = self.auth_token.read().await;
        if let Some(token) = token.as_ref() {
            token.cancel();
        }
    }

    /// Subscribe to the [AuthStatus] stream, returning events from interactive authentications.
    pub fn subscribe_auth_status(&self) -> watch::Receiver<AuthStatus> {
        self.auth_status_tx.subscribe()
    }

    /// Subscribe to the [ServerStatus] stream, returning status changes from ALL status changes for ALL servers.
    pub fn subscribe_server_status(&self) -> broadcast::Receiver<(Url, ServerStatus)> {
        self.server_status_tx.subscribe()
    }
}
