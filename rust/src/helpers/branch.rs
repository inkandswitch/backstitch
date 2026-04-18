use autosurgeon::{Hydrate, Reconcile};
use samod::DocumentId;
use std::collections::HashMap;

use crate::helpers::history_ref::HistoryRef;

/// The current version of the branch doc schema.
/// Bump this when making a breaking change to what we write into a branch doc.
/// Version 0: initial version
/// Version 1: file entries carry an md5 `hash` field; enables the fast hash-based slow diff.
pub const BRANCH_DOC_VERSION: u32 = 1;

#[derive(Debug, Clone, Reconcile, Hydrate, PartialEq)]
pub struct FileEntry {
    pub content: Option<String>,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Reconcile, Hydrate, PartialEq)]
pub struct GodotProjectDoc {
    pub files: HashMap<String, FileEntry>,
    pub state: HashMap<String, HashMap<String, String>>,
}

#[derive(Debug, Clone, Reconcile, Hydrate, PartialEq)]
pub struct BranchesMetadataDoc {
    #[autosurgeon(with = "crate::helpers::autosurgeon_utils::autosurgeon_doc_id")]
    pub main_doc_id: DocumentId,
    #[autosurgeon(with = "crate::helpers::autosurgeon_utils::autosurgeon_branch_map")]
    pub branches: HashMap<DocumentId, Branch>,
}

#[derive(Debug, Clone, Reconcile, Hydrate, PartialEq)]
pub struct Branch {
    /// The name of the branch.
    pub name: String,
    /// The [DocumentId] of the branch.
    #[autosurgeon(with = "crate::helpers::autosurgeon_utils::autosurgeon_doc_id")]
    pub id: DocumentId,
    /// The [HistoryRef] that we forked this branch off of.
    /// Guaranteed to exist on every branch that isn't the main branch.
    pub forked_from: Option<HistoryRef>,
    /// The [HistoryRef] of the branch we're targetting to merge into.
    /// Indicates that this is a merge preview branch.
    pub merge_into: Option<HistoryRef>,
    /// The [HistoryRef] of the heads we're reverting to.
    /// Indicates that this is a revert preview branch.
    /// Note that the branch in the ref will be the same as forked_from.
    pub reverted_to: Option<HistoryRef>,
    /// The name of the user that created this branch.
    pub created_by: Option<String>,
}
