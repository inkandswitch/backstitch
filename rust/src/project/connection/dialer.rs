use std::pin::Pin;

use futures::{Sink, SinkExt, Stream, StreamExt, TryStreamExt};
use reqwest::StatusCode;
use samod::{Dialer, Transport, websocket::WsMessage};
use secrecy::{ExposeSecret, SecretString};
use thiserror::Error;
use tungstenite::client::IntoClientRequest;
use url::Url;

#[derive(Error, Debug)]
pub enum DialerError {
    #[error("network: {0}")]
    Network(String),
}

/// A [Dialer] that supports websocket dialing/upgrading, as well as a bearer token
// Adapted from samod::TungsteniteDialer
pub struct AuthenticatedTungsteniteDialer {
    url: Url,
    bearer_token: Option<SecretString>,
}

impl AuthenticatedTungsteniteDialer {
    /// Create a new `TungsteniteDialer` for the given URL.
    pub fn new(url: Url, bearer_token: Option<SecretString>) -> Self {
        Self { url, bearer_token }
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
        let token = self.bearer_token.clone();
        Box::pin(async move {
            let mut request = url.as_str().into_client_request()?;

            if let Some(token) = token {
                request.headers_mut().insert(
                    tungstenite::http::header::AUTHORIZATION,
                    tungstenite::http::HeaderValue::from_str(&format!(
                        "Bearer {}",
                        token.expose_secret().to_string()
                    ))?,
                );
            }

            let (ws, _response) = match tokio_tungstenite::connect_async(request).await {
                Ok(res) => res,
                Err(e) => {
                    match &e {
                        tungstenite::Error::Http(response) => match response.status() {
                            // TODO: handle this case...
                            StatusCode::UNAUTHORIZED => {}
                            _ => {}
                        },
                        _ => {}
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
