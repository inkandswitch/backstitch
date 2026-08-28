use std::{sync::Arc, time::Duration};

use futures::{Stream, StreamExt};
use samod::{BackoffConfig, DialerEvent, DialerHandle, Repo, Stopped};
use thiserror::Error;
use tokio::{
    pin, select,
    sync::{Mutex, broadcast},
};
use tokio_stream::wrappers::BroadcastStream;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{
    auth::server_manager::{ServerError, ServerManager},
    helpers::spawn_utils::spawn_named,
    project::connection::dialer::AuthenticatedTungsteniteDialer,
};

mod dialer;

/// Connects a repo to the remote server's sync endpoint. Shuts down when dropped.
#[derive(Debug)]
pub struct RemoteConnection {
    inner: RemoteConnectionInner,
}

impl Drop for RemoteConnection {
    // Stop the connection on drop
    fn drop(&mut self) {
        self.inner.shutdown.cancel()
    }
}

#[derive(Debug, Clone)]
pub struct RemoteConnectionInner {
    server_manager: ServerManager,
    repo: Repo,
    shutdown: CancellationToken,
    // ensure no two connections can exist simultaneously
    connection_lock: Arc<Mutex<()>>,
    // ensure no connections can be started simultaneously
    connection_start_lock: Arc<Mutex<()>>,
    // simply provide data protection over ConnectionInfo
    connection_info: Arc<Mutex<Option<ConnectionInfo>>>,
    events_tx: broadcast::Sender<RemoteConnectionEvent>,
}

#[derive(Debug)]
struct ConnectionInfo {
    token: CancellationToken,
    handle: Option<DialerHandle>,
}

#[derive(Debug, Clone)]
pub enum RemoteConnectionEvent {
    /// We've successfully connected.
    Connected { username: Option<String> },
    /// A connection failed, and we will retry.
    Failed,
    /// A connection was canceled, and we will not retry.
    Cancelled,
}

#[derive(Error, Debug)]
pub enum RemoteConnectionError {
    #[error(transparent)]
    RepoStopped(#[from] Stopped),
    #[error(transparent)]
    Server(#[from] ServerError),
    #[error("the server is not authenticated, so we can't connect.")]
    NotAuthenticated,
}

impl RemoteConnection {
    /// Initialize the [RemoteConnection] module
    pub fn new(repo: Repo, server_manager: ServerManager) -> Self {
        let (events_tx, _) = broadcast::channel(5000);
        Self {
            inner: RemoteConnectionInner {
                server_manager,
                repo,
                shutdown: Default::default(),
                connection_lock: Default::default(),
                connection_start_lock: Default::default(),
                connection_info: Default::default(),
                events_tx,
            },
        }
    }

    /// Begins trying to connect to a server. Must be auth'd first.
    /// If there's already a connection, drops the existing connection.
    /// Returns whether we successfully immediately connected on our first try.
    /// If there's ANY redialing needed, returns false. Will continue attempting to connect.
    pub async fn connect(&self, url: &Url) -> Result<bool, RemoteConnectionError> {
        // keep the start lock through this method so nobody can connect two servers simultaneously
        let mut _start_guard = self.inner.connection_start_lock.lock().await;

        // cancel the old token
        {
            let info = self.inner.connection_info.lock().await;
            if let Some(info) = info.as_ref() {
                info.token.cancel();
            }
        }
        // ensure we've gracefully disconnected by the time we reconnect
        let connection_guard = self.inner.connection_lock.lock().await;

        // make a new token now that nobody's relying on the old info struct
        let tok = CancellationToken::new();
        {
            let mut info = self.inner.connection_info.lock().await;
            *info = Some(ConnectionInfo {
                token: tok.clone(),
                handle: None,
            });
        }

        let inner = self.inner.clone();
        let events = self.events();

        // drop the connection guard immediately so a new one can start.
        drop(connection_guard);
        let url = url.clone();
        spawn_named("loop_connection", async move {
            // this goes forever until canceled
            inner.loop_connection(&url, tok).await;
        });

        // We want to check for a single event before we return
        pin!(events);
        Ok(match events.next().await {
            Some(event) => match event {
                RemoteConnectionEvent::Connected { .. } => true,
                RemoteConnectionEvent::Failed => false,
                // shouldn't happen except maybe during total shutdown
                RemoteConnectionEvent::Cancelled => false,
            },
            // idk when this would happen; it shouldn't
            _ => {
                tracing::error!("Event stream went null; this shouldn't happen");
                false
            }
        })
    }

    /// Disconnect from a server, if there's a connection.
    pub async fn disconnect(&self) {
        let mut _start_guard = self.inner.connection_start_lock.lock().await;
        {
            let mut info = self.inner.connection_info.lock().await;
            if let Some(info) = info.take() {
                info.token.cancel();
            }
        }
        // ensure we've gracefully disconnected by the time we return
        let _connection_guard = self.inner.connection_lock.lock().await;
    }

    /// Subscribe to future events.
    pub fn events(&self) -> impl Stream<Item = RemoteConnectionEvent> + Send + 'static {
        BroadcastStream::new(self.inner.events_tx.subscribe())
            .filter_map(|result| async move { result.ok() })
    }

    /// Get the current status of the remote connection.
    /// Returns true if we're connected via WebSockets and ready to sync.
    pub async fn is_connected(&self) -> bool {
        let conn = self.inner.connection_info.lock().await;
        conn.as_ref()
            .is_some_and(|c| c.handle.as_ref().is_some_and(|h| h.is_connected()))
    }

    /// Returns whether we have a started connection at all, even if it's still connecting.
    /// This will be true if connect() has been called at all. Returns false when disconnect()
    /// has been called, up until another connect() has been called.
    pub async fn has_connection(&self) -> bool {
        self.inner.connection_info.lock().await.is_some()
    }
}

impl RemoteConnectionInner {
    /// Constantly retries to connect to the server and auth.
    // TODO (Subduction): This actually gets WAY simpler once we don't have to use dialers omg
    async fn loop_connection(&self, url: &Url, token: CancellationToken) {
        // We hold this for the entirety of the connection loop.
        // Therefore, we ensure NO two connections can exist simultaneously.
        // It will be dropped when the token is canceled and a graceful shutdown has completed.
        let _guard = self.connection_lock.lock().await;

        loop {
            // If all goes well, this will hang forever
            match self.handle_connection(url, token.clone()).await {
                Ok(_) => {}
                Err(e) => tracing::error!("Error connceting: {e:?}"),
            }

            if self.shutdown.is_cancelled() || token.is_cancelled() {
                let _ = self.events_tx.send(RemoteConnectionEvent::Cancelled);
                break;
            }

            // If something goes wrong, log it and try again later
            let _ = self.events_tx.send(RemoteConnectionEvent::Failed);
            select! {
                _ = self.shutdown.cancelled() => {
                    break;
                }
                _ = token.cancelled() => {
                    break;
                },
                _ = tokio::time::sleep(Duration::from_millis(5000)) => {}
            }
        }
    }

    // The inner layer of the connection -- creates a dialer and responds to events.
    // If this fails, we call handle_connection again after a delay.
    async fn handle_connection(
        &self,
        url: &Url,
        token: CancellationToken,
    ) -> Result<(), RemoteConnectionError> {
        tracing::debug!("Handshaking with server...");
        let server_info = self.server_manager.handshake(url).await?;
        tracing::debug!("Authenticating user...");
        let user_info = self
            .server_manager
            .try_authenticate(&server_info)
            .await
            .ok_or(RemoteConnectionError::NotAuthenticated)?;

        // Set HTTP to WS
        let mut url = server_info.sync_url.clone();
        url.set_scheme(match url.scheme() {
            "http" => "ws",
            "https" => "wss",
            _ => panic!("Could not initialize server connection; the URL {url} has an invalid scheme (must be http:// or https://)")
        }).unwrap();

        // Dial
        tracing::debug!("Starting connection...");
        let dialer = Arc::new(AuthenticatedTungsteniteDialer::new(url.clone()));
        dialer.set_bearer_token(user_info.bearer_token()).await;
        let handle = self.repo.dial(BackoffConfig::default(), dialer.clone())?;

        // set the handle now that we've created it
        {
            let mut info = self.connection_info.lock().await;
            if let Some(info) = info.as_mut() {
                info.handle = Some(handle.clone());
            } else {
                // if this is null, someone's trying to cancel us, so close things
                handle.close();
                return Ok(());
            }
        }

        let mut handle_events = handle.events();

        loop {
            select! {
                _ = token.cancelled() => {
                    tracing::debug!("Closing handle...");
                    handle.close();
                    break;
                }
                _ = self.shutdown.cancelled() => {
                    tracing::debug!("Closing handle...");
                    handle.close();
                    token.cancel();
                    break;
                }
                _ = dialer.auth_failed() => {
                    tracing::warn!("Authentication failed!");
                    // We can try and reauthenticate our bearer token, only if it's expired.
                    match self.server_manager.try_reauthenticate(&server_info).await {
                        Some(info) => {
                            tracing::info!("Retrying auth with new token...");
                            dialer.set_bearer_token(info.bearer_token()).await;
                        },
                        None => {
                            tracing::debug!("Closing handle...");
                            handle.close();
                            break;
                        }
                    }
                }
                event = handle_events.next() => {
                    tracing::debug!("Dialer event: {event:?}");
                    if let Some(e) = event {
                        match e {
                            DialerEvent::Connected { .. } => {
                                let _ = self.events_tx.send(RemoteConnectionEvent::Connected { username: user_info.username() });
                            },
                            // send the event -- samod's dialer will keep trying
                            DialerEvent::Disconnected { .. } => {
                                let _ = self.events_tx.send(RemoteConnectionEvent::Failed);
                            },
                            // we don't care about logging these for now
                            DialerEvent::Reconnecting { .. } => continue,
                            // we break out, and the outer loop handles sending Failed.
                            DialerEvent::MaxRetriesReached => {
                                tracing::debug!("Closing handle...");
                                handle.close();
                                break;
                            },
                        }
                    }
                }
            }
        }

        Ok(())
    }
}
