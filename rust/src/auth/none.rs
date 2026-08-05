use async_trait::async_trait;
use futures::{
    StreamExt,
    stream::{self, BoxStream},
};

use crate::auth::server_manager::{AuthError, AuthStatus, Authenticator, UserInfo};

#[derive(Clone)]
pub struct NoneUserInfo {
    name: String,
}

impl UserInfo for NoneUserInfo {
    fn username(&self) -> String {
        self.name.clone()
    }

    fn is_valid(&self) -> bool {
        true
    }

    fn clone_box(&self) -> Box<dyn UserInfo> {
        Box::new(self.clone())
    }
}

pub struct NoneAuthenticator {}

impl NoneAuthenticator {
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait]
impl Authenticator for NoneAuthenticator {
    async fn authenticate(&self) -> Result<Box<dyn UserInfo>, Box<dyn AuthError>> {
        // TODO: force the user to provide a name; get a name from storage; etc
        return Ok(Box::new(NoneUserInfo {
            name: "TEMP".to_string(),
        }) as Box<dyn UserInfo>);
    }

    fn subscribe_status(&self) -> BoxStream<'static, AuthStatus> {
        stream::once(async { AuthStatus::Ok })
            .chain(stream::pending())
            .boxed()
    }
}
