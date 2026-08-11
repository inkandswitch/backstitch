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
        HashMap<blake3::Hash, oneshot::Sender<Result<AuthResult, RedirectServerError>>>,
    >,
>;

const HTML_TEMPLATE: &str = include_str!("./html_template.html");

enum AuthResult {
    Login(AuthorizationCode),
    Logout,
}

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
            .route("/auth/oidc/callback", get(Self::login))
            .route("/auth/oidc/logged_out", get(Self::logged_out))
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
        })
    }

    async fn login(
        Query(params): Query<RedirectParams>,
        State(pending_auths): State<PendingAuths>,
    ) -> Html<String> {
        // State should always be there -- if not, we're for sure invalid.
        let Some(state) = params.state else {
            return Self::html_error(
                "Login",
                "The state query parameter is required to authenticate",
            );
        };

        let state = CsrfToken::new(state.clone());

        let tx = {
            let mut pending_auths = pending_auths.lock().unwrap();
            pending_auths.remove(&blake3::hash(state.secret().as_bytes()))
        };
        let Some(tx) = tx else {
            return Self::html_error(
                "Login",
                "Backstitch isn't waiting for us to authenticate with the provided state!",
            );
        };

        if let Some(error) = params.error {
            // receiever dropping is OK actually; that just means the waiter stopped waiting.
            let _ = tx.send(Err(RedirectServerError::AuthFailure(error.clone())));
            return Self::html_error("Login", &error);
        }

        let Some(code) = params.code else {
            let _ = tx.send(Err(RedirectServerError::NoAuthorizationCode));
            return Self::html_error("Login", "No authorization code was received!");
        };

        let code = AuthorizationCode::new(code);
        let _ = tx.send(Ok(AuthResult::Login(code)));
        tracing::info!("Successfully authenticated!");
        Self::html("Login Successful", "You may now close this tab.")
    }

    async fn logged_out(
        Query(params): Query<RedirectParams>,
        State(pending_auths): State<PendingAuths>,
    ) -> Html<String> {
        // State should always be there -- if not, we're for sure invalid.
        let Some(state) = params.state else {
            return Self::html_error("Logout", "The state query parameter is required to logout");
        };

        let state = CsrfToken::new(state.clone());

        let tx = {
            let mut pending_auths = pending_auths.lock().unwrap();
            pending_auths.remove(&blake3::hash(state.secret().as_bytes()))
        };
        let Some(tx) = tx else {
            return Self::html_error(
                "Logout",
                "Backstitch isn't waiting for us to logout with the provided state!",
            );
        };

        if let Some(error) = params.error {
            // receiever dropping is OK actually; that just means the waiter stopped waiting.
            let _ = tx.send(Err(RedirectServerError::AuthFailure(error.clone())));
            return Self::html_error("Logout", &error);
        }

        let _ = tx.send(Ok(AuthResult::Logout));
        tracing::info!("Successfully logged out!");
        Self::html("Logout Successful", "You may now close this tab.")
    }

    fn html_error(task: &str, message: &str) -> Html<String> {
        tracing::error!("Error in redirect server: {message}");
        return Self::html(&format!("{task} Error:"), message);
    }

    fn html(header: &str, body: &str) -> Html<String> {
        let header = html_escape::encode_text(header);
        let body = html_escape::encode_text(body);
        return Html(
            HTML_TEMPLATE
                .replace("{{HEADER}}", &format!("{header}"))
                .replace("{{BODY}}", &format!("{body}")),
        );
    }

    pub async fn wait_for_login(
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
        match result? {
            AuthResult::Login(authorization_code) => Ok(authorization_code),
            AuthResult::Logout => Err(RedirectServerError::AuthFailure(
                "wrong type of authorization (logout)".to_string(),
            )),
        }
    }

    pub async fn wait_for_logout(&self, state: CsrfToken) -> Result<(), RedirectServerError> {
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
        match result? {
            AuthResult::Logout => Ok(()),
            AuthResult::Login(_) => Err(RedirectServerError::AuthFailure(
                "wrong type of authorization (logout)".to_string(),
            )),
        }
    }
}
