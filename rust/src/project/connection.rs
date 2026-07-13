use std::str::FromStr;

use futures::{Stream, StreamExt};
use subduction_core::handshake::audience::Audience;
use subduction_crypto::signer::memory::MemorySigner;
use subduction_websocket::{
    tokio::client::{ClientConnectError, TokioWebSocketClient},
    websocket::{KeepAlive, KeepAliveTask},
};
use thiserror::Error;
use tokio::select;
use tokio_tungstenite::tungstenite::http::Uri;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{helpers::spawn_utils::spawn_named, project::doc_db::repo::Repo};

/// Connects a repo to the remote server. Shuts down when dropped.
#[derive(Debug)]
pub struct RemoteConnection {
    token: CancellationToken,
}

impl Drop for RemoteConnection {
    // Stop the connection on drop
    fn drop(&mut self) {
        self.token.cancel()
    }
}

#[derive(Error, Debug)]
pub enum RemoteConnectionError {
    #[error(transparent)]
    ClientConnect(#[from] ClientConnectError),
}

// TODO (Subduction): Add is_connected, reconnection stuff etc

impl RemoteConnection {
    /// Starts a connection to the server.
    pub async fn new(repo: &Repo, server_url: &Url) -> Result<Self, RemoteConnectionError> {
        if server_url.scheme() != "ws" && server_url.scheme() != "wss" {
            panic!(
                "Could not initialize server connection; the URL {server_url} has an invalid scheme (must be tcp://, ws://, or wss://)"
            );
        };

        let (client_ws, listener, sender, keepalive) = TokioWebSocketClient::new(
            Uri::from_str(server_url.as_str()).expect("URL not convertible to URI"),
            MemorySigner::from_bytes(&[0; 32]),
            Audience::discover(server_url.as_str().as_bytes()),
        )
        .await?;

        // run a subtask to cancel when requested
        let token = CancellationToken::new();
        {
            let token = token.clone();
            spawn_named("Remote connection", async move {
                keepalive.await;

                // TODO (Subduction): ALLOW CANCEL
            });
        }

        spawn_named("Listener", async move {
            listener.await;

            // TODO (Subduction): ALLOW CANCEL
        });

        spawn_named("Sender", async move {
            sender.await;

            // TODO (Subduction): ALLOW CANCEL
        });

        repo.subduction.add_connection(client_ws);

        Ok(Self {
            token,
            // dialer: handle,
        })
    }

    /// Subscribe to future events.
    // pub fn events(&self) -> impl Stream<Item = samod::DialerEvent> {
    //     self.dialer.events()
    // }

    /// Get the current status of the remote connection.
    pub fn is_connected(&self) -> bool {
        return true;
        // self.dialer.is_connected()
    }
}
