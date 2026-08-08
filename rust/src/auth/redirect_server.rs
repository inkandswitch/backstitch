use axum::{
    Router,
    extract::{Query, State},
    response::Html,
    routing::get,
};
use openidconnect::{AuthorizationCode, CsrfToken};
use serde::Deserialize;
use std::{collections::HashMap, net::SocketAddr, str::FromStr, sync::Arc};
use thiserror::Error;
use tokio::{net::TcpListener, sync::oneshot};
use tokio_util::sync::CancellationToken;

// the hashmap key is CSRFToken, hashed with blake3 for security.
// We use a synchronous mutex here because contention is rare and quick, and it works with the drop pattern we use later.
type PendingAuths = Arc<
    std::sync::Mutex<
        HashMap<blake3::Hash, oneshot::Sender<Result<AuthorizationCode, RedirectServerError>>>,
    >,
>;

#[derive(Error, Debug)]
pub enum RedirectServerError {
    #[error(transparent)]
    Recv(#[from] oneshot::error::RecvError),
    #[error("an error occurred while authenticating: {0}")]
    AuthFailure(String),
    #[error("no authorization code was received during authentication")]
    NoAuthorizationCode,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub struct RedirectServer {
    pending_auths: PendingAuths,
    token: CancellationToken,
    port: u16,
}

#[derive(Deserialize)]
struct RedirectParams {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

impl Drop for RedirectServer {
    fn drop(&mut self) {
        self.token.cancel();
    }
}

impl RedirectServer {
    pub async fn new(port: u16) -> Result<Self, RedirectServerError> {
        let pending_auth: PendingAuths = Default::default();
        let router = Router::new()
            .route("/", get(Self::redirect))
            .with_state(pending_auth.clone());

        let listener =
            TcpListener::bind(SocketAddr::from_str(&format!("127.0.0.1:{port}")).unwrap()).await?;

        let token = CancellationToken::new();
        {
            let token = token.clone();
            tokio::spawn(async move {
                // Although serve returns a result, the future never actually finishes.
                let _ = axum::serve(listener, router)
                    .with_graceful_shutdown(async move {
                        token.cancelled().await;
                    })
                    .await;
            });
        }

        Ok(Self {
            pending_auths: pending_auth,
            token,
            port,
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    async fn redirect(
        Query(params): Query<RedirectParams>,
        State(pending_auths): State<PendingAuths>,
    ) -> Html<String> {
        // State should always be there -- if not, we're for sure invalid.
        let Some(state) = params.state else {
            return Self::html_error("The state query parameter is required to authenticate");
        };

        let state = CsrfToken::new(state.clone());

        let tx = {
            let mut pending_auths = pending_auths.lock().unwrap();
            pending_auths.remove(&blake3::hash(state.secret().as_bytes()))
        };
        let Some(tx) = tx else {
            return Self::html_error(
                "Backstitch isn't waiting for us to authenticate with the provided state!",
            );
        };

        if let Some(error) = params.error {
            // receiever dropping is OK actually; that just means the waiter stopped waiting.
            let _ = tx.send(Err(RedirectServerError::AuthFailure(error.clone())));
            return Self::html_error(&format!("There was an error while authenticating: {error}"));
        }

        let Some(code) = params.code else {
            let _ = tx.send(Err(RedirectServerError::NoAuthorizationCode));
            return Self::html_error("No authorization code was received!");
        };

        let code = AuthorizationCode::new(code);
        let _ = tx.send(Ok(code));
        Self::html_success()
    }

    // TODO (oidc): it'd be wonderful to include_str!() some pretty HTML here.
    fn html_error(message: &str) -> Html<String> {
        tracing::error!("Error in redirect server: {message}");
        let message = html_escape::encode_text(message);
        return Html(format!(
            "<h2>Backstitch: Authentication Error</h2>\
            <p>{message}</p>\
            <p>Please return to Backstitch and try again.</p>"
        ));
    }

    fn html_success() -> Html<String> {
        tracing::info!("Successfully authenticated!");
        return Html(
            "<h2>Backstitch: Authentication successful</h2>\
                <p>You may now close this window.</p>"
                .to_string(),
        );
    }

    pub async fn wait_for_redirect(
        &self,
        state: CsrfToken,
    ) -> Result<AuthorizationCode, RedirectServerError> {
        struct Guard {
            state: CsrfToken,
            pending_auths: PendingAuths,
        }

        // This will run if the future is dropped
        impl Drop for Guard {
            fn drop(&mut self) {
                self.pending_auths
                    .lock()
                    .unwrap()
                    .remove(&blake3::hash(self.state.secret().as_bytes()));
            }
        }

        let guard = Guard {
            state: state.clone(),
            pending_auths: self.pending_auths.clone(),
        };

        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending_auths.lock().unwrap();

            if pending
                .insert(blake3::hash(state.secret().as_bytes()), tx)
                .is_some()
            {
                panic!("duplicate CSRF token?!?! some funny business is afoot...")
            }
        }

        let result = rx.await?;
        drop(guard);
        result
    }
}
