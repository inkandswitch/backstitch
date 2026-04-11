use futures::Stream;
use samod::{BackoffConfig, DialerHandle, Repo, Url};
use tokio_util::sync::CancellationToken;

use crate::helpers::spawn_utils::spawn_named;

/// Connects a repo to the remote server. Shuts down when dropped.
#[derive(Debug)]
pub struct RemoteConnection {
    dialer: DialerHandle,
    token: CancellationToken,
}

impl Drop for RemoteConnection {
    // Stop the connection on drop
    fn drop(&mut self) {
        self.token.cancel()
    }
}

impl RemoteConnection {
    /// Starts a connection to the server.
    pub async fn new(repo: Repo, server_url: Url) -> Option<Self> {
        let handle = if server_url.scheme() == "ws" || server_url.scheme() == "wss" {
            repo.dial_websocket(server_url, BackoffConfig::default())
                .ok()?
        } else if server_url.scheme() == "tcp" {
            repo.dial_tcp(server_url, BackoffConfig::default()).ok()?
        } else {
            tracing::error!(
                "Could not initialize server connection; the URL {server_url} has an invalid scheme (must be tcp://, ws://, or wss://)"
            );
            return None;
        };

        // run a subtask to cancel when requested
        let token = CancellationToken::new();
        {
            let handle = handle.clone();
            let token = token.clone();
            spawn_named("Remote connection", async move {
                token.cancelled().await;
                handle.close();
            });
        }

        Some(Self {
            token,
            dialer: handle,
        })
    }

    /// Subscribe to future events.
    pub fn events(&self) -> impl Stream<Item = samod::DialerEvent> {
        self.dialer.events()
    }

    /// Get the current status of the remote connection.
    pub fn is_connected(&self) -> bool {
        self.dialer.is_connected()
    }
}
