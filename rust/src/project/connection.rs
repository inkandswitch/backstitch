use std::{str::FromStr, sync::Arc, time::Duration};

use axum::http::Uri;
use futures::{Stream, StreamExt};
use subduction_core::{
    connection::ConnectionDisallowed,
    handshake::{
        self,
        audience::{Audience, DiscoveryId},
    },
    peer::id::PeerId,
    subduction::error::AddConnectionError,
    timeout::call::CallTimeout,
};
use subduction_crypto::signer::memory::MemorySigner;
use subduction_websocket::{
    error::DisconnectionError,
    tokio::client::{ClientConnectError, TokioWebSocketClient},
    websocket::{KeepAlive, KeepAliveOutcome, KeepAliveTask},
};
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
    project::repo::Repo,
};

/// Connects a repo to the remote server's sync endpoint. Shuts down when dropped.
#[derive(Debug)]
pub struct RemoteConnection {
    inner: RemoteConnectionInner,
}

// TODO (subd): How to get this from subduction? :(
#[derive(Debug, Clone)]
pub struct ConnectionInfo {
    /// Last time we received a message from this peer
    pub last_received: Option<chrono::DateTime<chrono::Utc>>,
    /// Last time we sent a message to this peer
    pub last_sent: Option<chrono::DateTime<chrono::Utc>>,
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
    // simply provide data protection over RemoteConnectionInfo
    connection_info: Arc<Mutex<Option<RemoteConnectionInfo>>>,
    events_tx: broadcast::Sender<RemoteConnectionEvent>,
}

#[derive(Debug)]
struct RemoteConnectionInfo {
    token: CancellationToken,
    is_connected: bool,
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
    Server(#[from] ServerError),
    #[error("the server is not authenticated, so we can't connect.")]
    NotAuthenticated,
    #[error(transparent)]
    Disconnect(#[from] DisconnectionError),
    #[error(transparent)]
    Connect(#[from] ClientConnectError),
    #[error(transparent)]
    AddConnection(#[from] AddConnectionError<!>),
}

// todo (subd): Add is_connected, reconnection stuff etc

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
            *info = Some(RemoteConnectionInfo {
                token: tok.clone(),
                is_connected: false,
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
        conn.as_ref().is_some_and(|c| c.is_connected)
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
    // todo (subd): This actually gets WAY simpler once we don't have to use dialers omg
    async fn loop_connection(&self, url: &Url, token: CancellationToken) {
        // We hold this for the entirety of the connection loop.
        // Therefore, we ensure NO two connections can exist simultaneously.
        // It will be dropped when the token is canceled and a graceful shutdown has completed.
        let _guard = self.connection_lock.lock().await;

        loop {
            // If all goes well, this will hang forever
            match self.handle_connection(url, token.clone()).await {
                Ok(_) => {}
                Err(e) => tracing::error!("Error connecting: {e:?}"),
            }

            // If we intentionally shut down, send a special event.
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

        tracing::debug!("Starting connection...");
        let subd = self.repo.subduction();
        // todo (subd): bearer token; reintroduce auth failure pain case and try_reauthenticate
        let (client_ws, listener_fut, sender_fut, keepalive_task) = TokioWebSocketClient::new(
            Uri::from_str(&url.to_string()).expect("URL to URI conversion broken..."),
            subd.signer().clone(),
            Audience::Discover(DiscoveryId::new("backstitch_sync_server".as_bytes())),
        )
        .await?;

        subd.add_connection(client_ws.clone()).await?;

        {
            let mut info = self.connection_info.lock().await;
            if let Some(info) = info.as_mut() {
                info.is_connected = true;
            }
        }

        // TODO: add auth'd username here
        let _ = self
            .events_tx
            .send(RemoteConnectionEvent::Connected { username: None });

        let shutdown = self.shutdown.clone();
        let t = tokio::task::spawn(async move {
            select! {
                _ = shutdown.cancelled() => {}
                _ = token.cancelled() => {}
                res = listener_fut => {
                    match res {
                        Ok(()) => tracing::debug!("Listener exiting successfully..."),
                        Err(e) => tracing::error!("Listener exiting with error: {e}"),
                    }
                }
                res = sender_fut => {
                    match res {
                        Ok(()) => tracing::debug!("Sender exiting successfully..."),
                        Err(e) => tracing::error!("Sender exiting with error: {e}"),
                    }
                }
                res = keepalive_task => {
                    match res {
                        KeepAliveOutcome::ConnectionClosed => tracing::debug!("Keepalive: connection closed"),
                        KeepAliveOutcome::Timeout { missed } => tracing::error!("Keepalive: timeout ({missed} missed)"),
                        KeepAliveOutcome::StaleNoPong { unanswered } => tracing::error!("Keepalive: no pong ({unanswered} unanswered)"),
                    }
                }
            }
        });

        tracing::debug!("full syncing...");
        let _ = subd
            .full_sync_with_all_peers(CallTimeout::TimeoutMillis(10000))
            .await;
        tracing::debug!("done full syncing");

        t.await;

        {
            let mut info = self.connection_info.lock().await;
            if let Some(info) = info.as_mut() {
                info.is_connected = false;
            }
        }

        subd.disconnect(&client_ws).await?;
        Ok(())
    }
}
