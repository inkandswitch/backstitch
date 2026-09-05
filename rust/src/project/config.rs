use indexmap::IndexSet;
use sedimentree_core::id::SedimentreeId;
use std::{path::Path, str::FromStr};
use url::Url;

use crate::project::config::file_backed_toml::FileBackedToml;

mod file_backed_toml;

const CONFIG_FILE_NAME: &str = "backstitch.cfg";
const USER_DIR_NAME: &str = "backstitch_plugin";
const SECTION: &str = "backstitch";

/// Backstitch's configuration, split between per-project settings that live alongside the project
/// and per-user settings that live outside of it.
#[derive(Debug, Clone)]
pub struct Config {
    // TODO: We actually want a third here -- we need a user config in .backstitch for storing the checked out branch ID.
    project: FileBackedToml,
    user: FileBackedToml,
}

impl Config {
    /// The project config lives at `<project_dir>/backstitch.cfg`, and the user config at
    /// `<user_data_dir>/backstitch_plugin/backstitch.cfg`.
    pub fn new(project_dir: &Path, user_data_dir: &Path) -> Self {
        Self {
            project: FileBackedToml::new(&project_dir.join(CONFIG_FILE_NAME)),
            user: FileBackedToml::new(&user_data_dir.join(USER_DIR_NAME).join(CONFIG_FILE_NAME)),
        }
    }

    pub async fn project_doc_id(&self) -> Option<SedimentreeId> {
        SedimentreeId::from_str(&Self::get_string(&self.project, "project_doc_id").await?)
            .inspect_err(|e| tracing::error!("Couldn't get project_doc_id: {e}"))
            .ok()
    }

    pub async fn set_project_doc_id(&self, value: Option<&SedimentreeId>) {
        Self::set_string(
            &self.project,
            "project_doc_id",
            value.map(|id| id.to_string()).as_deref(),
        )
        .await
    }

    pub async fn server_url(&self) -> Option<Url> {
        let mut str = Self::get_string(&self.project, "server_url").await?;
        // Remap legacy TCP to new HTTP
        if str.contains("alpha.backstitch.dev:8085") {
            str = "https://alpha.backstitch.dev/".to_string()
        }
        Url::parse(&str)
            .inspect_err(|e| tracing::error!("Invalid URL: {str}, {e}"))
            .ok()
    }

    pub async fn set_server_url(&self, value: Option<&Url>) {
        Self::set_string(
            &self.project,
            "server_url",
            value.map(|url| url.to_string()).as_deref(),
        )
        .await
    }

    pub async fn available_servers(&self) -> IndexSet<Url> {
        let Some(servers) = Self::get_string(&self.project, "available_servers").await else {
            return Default::default();
        };
        servers
            .split(",")
            .map(|s| {
                // Remap legacy TCP to new HTTP
                if s.contains("alpha.backstitch.dev:8085") {
                    "https://alpha.backstitch.dev/"
                } else {
                    s
                }
            })
            .filter_map(|str| {
                Url::parse(str)
                    .inspect_err(|e| {
                        tracing::error!("Invalid URL in available_servers: {str}, {e}")
                    })
                    .ok()
            })
            .collect()
    }

    pub async fn set_available_servers(&self, servers: &IndexSet<Url>) {
        Self::set_string(
            &self.project,
            "available_servers",
            Some(
                &servers
                    .into_iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<String>>()
                    .join(","),
            ),
        )
        .await
    }

    pub async fn checked_out_branch_doc_id(&self) -> Option<SedimentreeId> {
        SedimentreeId::from_str(
            &Self::get_string(&self.project, "checked_out_branch_doc_id").await?,
        )
        .inspect_err(|e| tracing::error!("Couldn't get project_doc_id: {e}"))
        .ok()
    }

    pub async fn set_checked_out_branch_doc_id(&self, value: Option<&SedimentreeId>) {
        Self::set_string(
            &self.project,
            "checked_out_branch_doc_id",
            value.map(|id| id.to_string()).as_deref(),
        )
        .await
    }

    // TODO: Merge this with auth pipeline somehow...
    pub async fn user_name(&self) -> Option<String> {
        Self::get_string(&self.user, "user_name").await
    }

    pub async fn set_user_name(&self, val: Option<&str>) {
        Self::set_string(&self.user, "user_name", val).await
    }

    async fn get_string(toml: &FileBackedToml, key: &str) -> Option<String> {
        match toml.get(SECTION, key).await {
            Ok(None) => return None,
            Ok(Some(value)) => match value {
                toml::Value::String(s) => return (!s.is_empty()).then_some(s),
                _ => tracing::error!("Error getting {key}: value is not a string!"),
            },
            Err(e) => tracing::error!("Error getting {key}: {e}"),
        }
        None
    }

    async fn set_string(toml: &FileBackedToml, key: &str, value: Option<&str>) {
        match toml
            .set(
                SECTION,
                key,
                value.map(|s| toml::Value::String(s.to_string())),
            )
            .await
        {
            Ok(_) => {}
            Err(e) => tracing::error!("Error setting {key}: {e}"),
        }
    }
}
