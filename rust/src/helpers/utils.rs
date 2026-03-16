use std::{collections::HashSet, fmt, path::Path, str::FromStr, time::SystemTime};

use crate::{diff::differ::ProjectDiff, helpers::branch::Branch};
use automerge::{
    ChangeHash,
    transaction::{CommitOptions, Transaction},
};
use chrono::{DateTime, Local, Locale, TimeZone};
use samod::DocumentId;
use serde::{Deserialize, Serialize};

pub(crate) fn get_changed_files(patches: &Vec<automerge::Patch>) -> HashSet<String> {
    let mut changed_files = HashSet::new();

    // log all patches
    for patch in patches.iter() {
        let first_key = match patch.path.get(0) {
            Some((_, prop)) => match prop {
                automerge::Prop::Map(string) => string,
                _ => continue,
            },
            _ => continue,
        };

        // get second key
        let second_key = match patch.path.get(1) {
            Some((_, prop)) => match prop {
                automerge::Prop::Map(string) => string,
                _ => continue,
            },
            _ => continue,
        };

        if first_key == "files" {
            changed_files.insert(second_key.to_string());
        }

        // tracing::debug!("changed files: {:?}", changed_files);
    }

    return changed_files;
}

pub(crate) fn parse_automerge_url(url: &str) -> Option<DocumentId> {
    const PREFIX: &str = "automerge:";
    if !url.starts_with(PREFIX) {
        return None;
    }

    let hash = &url[PREFIX.len()..];
    DocumentId::from_str(hash).ok()
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct MergeMetadata {
    pub merged_branch_id: DocumentId,
    pub forked_at_heads: Vec<ChangeHash>,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
pub enum ChangeType {
    Added,
    Removed,
    Modified,
}

impl fmt::Display for ChangeType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ChangeType::Added => write!(f, "added"),
            ChangeType::Removed => write!(f, "removed"),
            ChangeType::Modified => write!(f, "modified"),
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct ChangedFile {
    pub change_type: ChangeType,
    pub path: String,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct CommitMetadata {
    pub username: Option<String>,
    pub branch_id: Option<DocumentId>,
    pub merge_metadata: Option<MergeMetadata>,
    pub reverted_to: Option<Vec<ChangeHash>>,
    /// Changed files in this commit. Only valid for commits to branch documents.
    pub changed_files: Option<Vec<ChangedFile>>,
    /// Whether this change was created to initialize the repository.
    pub is_setup: Option<bool>,
}

pub(crate) fn commit_with_metadata(
    tx: Transaction,
    metadata: &CommitMetadata,
) -> Option<ChangeHash> {
    let timestamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;

    let message = serde_json::json!(metadata).to_string();

    tx.commit_with(
        CommitOptions::default()
            .with_message(message)
            .with_time(timestamp),
    )
    .0
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommitInfo {
    pub hash: ChangeHash,
    pub timestamp: i64,
    pub metadata: Option<CommitMetadata>,
    pub synced: bool,
    pub summary: String,
}

#[derive(Debug)]
pub struct BranchWrapper {
    pub state: Branch,
    pub children: Vec<DocumentId>,
}

#[derive(Debug)]
pub struct DiffWrapper {
    pub diff: ProjectDiff,
    pub title: String,
}

pub fn summarize_changes(author: &str, changes: &Vec<ChangedFile>) -> String {
    let added = get_summary_text(&changes, ChangeType::Added, None);
    let removed = get_summary_text(&changes, ChangeType::Removed, None);
    let modified = get_summary_text(&changes, ChangeType::Modified, Some("edited"));

    let strings: Vec<String> = [added, removed, modified]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect();

    match strings.len() {
        3 | 0 => format!("{author} made some changes"),
        2 => format!("{author} {} and {}", strings[0], strings[1]),
        1 => format!("{author} {}", strings[0]),
        _ => unreachable!(),
    }
}

fn get_summary_text(
    changes: &Vec<ChangedFile>,
    operation: ChangeType,
    display_operation: Option<&str>,
) -> String {
    let display = display_operation.unwrap_or(match operation {
        ChangeType::Added => "added",
        ChangeType::Removed => "removed",
        ChangeType::Modified => "modified",
    });

    let filtered: Vec<&ChangedFile> = changes
        .iter()
        .filter(|c| c.change_type == operation)
        .collect();

    if filtered.is_empty() {
        return String::new();
    }

    if filtered.len() == 1 {
        // Extract filename via std::path
        let filename = Path::new(&filtered[0].path)
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or(&filtered[0].path);

        return format!("{} {}", display, filename);
    }

    format!("{} {} files", display, filtered.len())
}

pub fn human_readable_timestamp(timestamp: i64) -> String {
    let now = Local::now();
    let dt: DateTime<Local> = Local.timestamp_opt(timestamp / 1000, 0).unwrap();
    let diff = now.signed_duration_since(dt);

    let secs = diff.num_seconds();

    if secs < 60 {
        format!("{}s ago", secs)
    } else if secs < 60 * 60 {
        format!("{}m ago", secs / 60)
    } else if secs < 60 * 60 * 24 {
        format!("{}h ago", secs / 3600)
    } else if secs < 60 * 60 * 24 * 9 {
        format!("{}d ago", secs / 86400)
    } else {
        dt.format_localized("%x", locale_from_system()).to_string()
    }
}

fn locale_from_system() -> Locale {
    // Convert BCP-47 to chrono format
    let locale = sys_locale::get_locale().unwrap_or("en-US".to_string());
    let normalized = locale
        .split('.')
        .next()
        .unwrap_or(&locale)
        .replace('-', "_");

    Locale::from_str(&normalized).unwrap_or(Locale::en_US)
}

pub fn exact_human_readable_timestamp(timestamp: i64) -> String {
    let dt = DateTime::from_timestamp(timestamp / 1000, 0);
    let datetime: DateTime<Local> = DateTime::from(dt.unwrap());
    return datetime.format("%Y-%m-%d %H:%M:%S").to_string();
}
