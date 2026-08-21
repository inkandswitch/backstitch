use async_trait::async_trait;
use secrecy::SecretString;

use crate::auth::server_manager::{AuthError, AuthStatus, Authenticator, UserInfo};

#[derive(Clone, Debug)]
pub struct NoneUserInfo {
    name: String,
}

impl UserInfo for NoneUserInfo {
    fn username(&self) -> String {
        self.name.clone()
    }

    fn subject(&self) -> String {
        self.name.clone()
    }

    fn email(&self) -> Option<String> {
        None
    }

    fn is_valid(&self) -> bool {
        true
    }

    fn bearer_token(&self) -> Option<SecretString> {
        None
    }

    fn clone_box(&self) -> Box<dyn UserInfo> {
        Box::new(self.clone())
    }
}

#[derive(Debug)]
pub struct NoneAuthenticator {}

impl NoneAuthenticator {
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait]
impl Authenticator for NoneAuthenticator {
    async fn interactive_authenticate(&self) -> Result<Box<dyn UserInfo>, Box<dyn AuthError>> {
        // TODO (oidc): force the user to provide a name; get a name from storage; etc
        Ok(Box::new(NoneUserInfo {
            name: "TEMP".to_string(),
        }) as Box<dyn UserInfo>)
    }
    async fn immediate_authenticate(
        &self,
    ) -> Result<Option<Box<dyn UserInfo>>, Box<dyn AuthError>> {
        Ok(Some(Box::new(NoneUserInfo {
            name: "TEMP".to_string(),
        }) as Box<dyn UserInfo>))
    }
    async fn deauthenticate(&self) -> Result<(), Box<dyn AuthError>> {
        Ok(())
    }

    async fn status_changed(&self) -> AuthStatus {
        // Wait forever lol
        std::future::pending::<()>().await;
        AuthStatus::Idle
    }

    fn provider(&self) -> String {
        "none".to_string()
    }
}
