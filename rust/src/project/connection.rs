use futures::{Stream, StreamExt};
use samod::{BackoffConfig, DialerHandle, Repo, Stopped, Url, websocket::TungsteniteDialer};
use secrecy::ExposeSecret;
use thiserror::Error;
use tokio::select;
use tokio_util::sync::CancellationToken;

use crate::{
    auth::{handshake::ServerInfo, server_manager::UserInfo},
    helpers::spawn_utils::spawn_named,
};

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

#[derive(Error, Debug)]
pub enum RemoteConnectionError {
    #[error(transparent)]
    RepoStopped(#[from] Stopped),
}

impl RemoteConnection {
    /// Starts a connection to the server.
    pub async fn new(
        repo: Repo,
        server_info: &ServerInfo,
        user_info: &Box<dyn UserInfo>,
    ) -> Result<Self, RemoteConnectionError> {
        // TODO (important): Detect authentication failures, re-auth, and reconnect.

        let mut url = server_info
            .url
            .join("sync")
            .expect("something went wrong in joining??");
        url.set_scheme(match server_url.scheme() {
            "http" => "ws",
            "https" => "wss",
            _ => panic!("Could not initialize server connection; the URL {url} has an invalid scheme (must be http:// or https://)")
        }).expect("something went wrong in scheme setting??");

        TungsteniteDialer::new(
            url,
            user_info
                .bearer_token()
                .map(|s| s.expose_secret().to_string()),
        );

        let handle = repo.dial_websocket(
            server_url.join("sync").expect("Weird URL parsing error"),
            BackoffConfig::default(),
        )?;

        // run a subtask to cancel when requested
        let token = CancellationToken::new();
        {
            let handle = handle.clone();
            let token = token.clone();
            spawn_named("Remote connection", async move {
                let mut events = handle.events();

                loop {
                    select! {
                        event = events.next() => {
                            tracing::debug!("Dialer event: {event:?}");
                        }
                        _ = token.cancelled() => {
                            handle.close();
                            break;
                        }
                    }
                }
            });
        }

        Ok(Self {
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
