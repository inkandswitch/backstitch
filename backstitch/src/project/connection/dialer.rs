use std::{pin::Pin, sync::Arc};

use axum::http::Uri;
use futures::{Sink, SinkExt, Stream, StreamExt, TryStreamExt};
use reqwest::StatusCode;
use samod::{Dialer, Transport, websocket::WsMessage};
use secrecy::{ExposeSecret, SecretString};
use thiserror::Error;
use tokio::sync::{Mutex, watch};
use tungstenite::{
    client::IntoClientRequest,
    handshake::client::{Request, generate_key},
};
use url::Url;

#[derive(Error, Debug)]
pub enum DialerError {
    #[error("network: {0}")]
    Network(String),
}

/// A [Dialer] that supports websocket dialing/upgrading, as well as a bearer token
// Adapted from samod::TungsteniteDialer
#[derive(Debug)]
pub struct AuthenticatedTungsteniteDialer {
    url: Url,
    bearer_token: Arc<Mutex<Option<SecretString>>>,
    auth_failed_tx: watch::Sender<bool>,
}

impl AuthenticatedTungsteniteDialer {
    /// Create a new `TungsteniteDialer` for the given URL.
    pub fn new(url: Url) -> Self {
        let (tx, _rx) = watch::channel(false);
        Self {
            url,
            bearer_token: Default::default(),
            auth_failed_tx: tx,
        }
    }

    pub async fn set_bearer_token(&self, bearer_token: Option<SecretString>) {
        let mut tok = self.bearer_token.lock().await;
        *tok = bearer_token;
        self.auth_failed_tx.send_replace(false);
    }

    pub async fn auth_failed(&self) {
        // Listen til we get an auth_failed; once it's failed once it's cooked for ever
        let mut rx = self.auth_failed_tx.subscribe();
        loop {
            if *rx.borrow() {
                return;
            }
            let _ = rx.changed().await;
        }
    }
}

impl Dialer for AuthenticatedTungsteniteDialer {
    fn url(&self) -> Url {
        self.url.clone()
    }

    fn connect(
        &self,
    ) -> Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<Transport, Box<dyn std::error::Error + Send + Sync + 'static>>,
                > + Send,
        >,
    > {
        let url = self.url.clone();
        let auth_failed_tx = self.auth_failed_tx.clone();
        let bearer_token = self.bearer_token.clone();
        Box::pin(async move {
            let mut request = into_client_request(&url)?;

            if let Some(token) = bearer_token.lock().await.clone() {
                request.headers_mut().insert(
                    tungstenite::http::header::AUTHORIZATION,
                    tungstenite::http::HeaderValue::from_str(&format!(
                        "Bearer {}",
                        token.expose_secret()
                    ))?,
                );
            }

            let (ws, _response) = match tokio_tungstenite::connect_async(request.clone()).await {
                Ok(res) => res,
                Err(e) => {
                    tracing::error!("error while dialing {e}");
                    if let tungstenite::Error::Http(response) = &e {
                        match response.status() {
                            StatusCode::UNAUTHORIZED => {
                                tracing::error!("UNAUTHORIZED");
                                auth_failed_tx.send_replace(true);
                            }
                            code => tracing::error!("HTTP error {code}"),
                        }
                    }
                    Err(e)?
                }
            };

            // Wrap tungstenite errors into NetworkError
            let ws = ws
                .map_err(|e| {
                    tracing::info!("Err1");
                    DialerError::Network(format!("error receiving websocket message: {}", e))
                })
                .sink_map_err(|e| {
                    tracing::info!("Err2");
                    DialerError::Network(format!("error sending websocket message: {}", e))
                });

            // Convert WebSocket messages to raw bytes
            let (msg_stream, msg_sink) = ws_to_bytes::<_, tungstenite::Message>(ws);

            Ok(Transport::new(msg_stream, msg_sink))
        })
    }
}

type BoxedBytesStream = futures::stream::BoxStream<'static, Result<Vec<u8>, DialerError>>;
fn ws_to_bytes<S, M>(
    stream: S,
) -> (
    BoxedBytesStream,
    impl Sink<Vec<u8>, Error = DialerError> + Send + Unpin,
)
where
    M: Into<WsMessage> + From<WsMessage> + Send + 'static,
    S: Sink<M, Error = DialerError> + Stream<Item = Result<M, DialerError>> + Send + 'static,
{
    let (sink, stream) = stream.split();

    let msg_stream = stream
        .filter_map::<_, Result<Vec<u8>, DialerError>, _>({
            move |msg| async move {
                let msg = match msg {
                    Ok(m) => m,
                    Err(e) => {
                        return Some(Err(DialerError::Network(format!(
                            "websocket receive error: {e}"
                        ))));
                    }
                };
                match msg.into() {
                    WsMessage::Binary(data) => Some(Ok(data)),
                    WsMessage::Close => {
                        tracing::debug!("websocket closing");
                        None
                    }
                    WsMessage::Ping(_) | WsMessage::Pong(_) => None,
                    WsMessage::Text(_) => Some(Err(DialerError::Network(
                        "unexpected string message on websocket".to_string(),
                    ))),
                }
            }
        })
        .boxed();

    let msg_sink = sink
        .sink_map_err(|e| DialerError::Network(format!("websocket send error: {e}")))
        .with(|msg| futures::future::ready(Ok::<_, DialerError>(WsMessage::Binary(msg).into())));

    (msg_stream, msg_sink)
}
use base64::{Engine, engine::general_purpose::STANDARD};
fn into_client_request(url: &Url) -> tungstenite::Result<Request> {
    let mut url = url.clone();
    let user = url.username().to_string();
    let pass = url.password().map(|s| s.to_string());
    url.set_username("")
        .map_err(|_| tungstenite::Error::Url(tungstenite::error::UrlError::NoHostName))?;
    url.set_password(None)
        .map_err(|_| tungstenite::Error::Url(tungstenite::error::UrlError::NoHostName))?;
    let url = &url.as_str().parse::<Uri>()?;
    let authority = url
        .authority()
        .ok_or(tungstenite::Error::Url(
            tungstenite::error::UrlError::NoHostName,
        ))?
        .as_str();
    let host = authority
        .find('@')
        .map(|idx| authority.split_at(idx + 1).1)
        .unwrap_or_else(|| authority);

    if host.is_empty() {
        return Err(tungstenite::Error::Url(
            tungstenite::error::UrlError::EmptyHostName,
        ));
    }

    let mut req = Request::builder()
        .method("GET")
        .header("Host", host)
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", generate_key());

    if let Some(pass) = pass {
        let credentials = STANDARD.encode(format!("{user}:{pass}"));
        req = req.header("Authorization", format!("Basic {credentials}"));
    }

    let req = req.uri(url).body(())?;
    Ok(req)
}
