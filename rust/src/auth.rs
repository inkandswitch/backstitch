use thiserror::Error;

use crate::auth::{handshake::ServerInfo, oidc::OidcUserInfo};

pub mod handshake;
pub mod oidc;
mod redirect_server;

#[derive(Error, Debug)]
pub enum AuthError {
    #[error(transparent)]
    Oidc(#[from] oidc::AuthError),
}

pub enum UserInfo {
    Oidc(OidcUserInfo),
    None,
}

/// Authenticate from our [ServerInfo] to a [UserInfo]
pub async fn authenticate(info: &ServerInfo) -> Result<UserInfo, AuthError> {
    Ok(match &info.auth {
        handshake::AuthConfig::Oidc(oidc_auth_config) => {
            UserInfo::Oidc(oidc::authenticate(oidc_auth_config).await?)
        }
        handshake::AuthConfig::None => UserInfo::None,
    })
}
