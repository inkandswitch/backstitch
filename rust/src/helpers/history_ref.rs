use automerge::ChangeHash;
use autosurgeon::{Hydrate, Reconcile};
use samod::DocumentId;
use serde::{Deserialize, Serialize};
use std::{fmt::Display, str::FromStr};

/// Compare Automerge heads without relying on the order returned by a document.
pub(crate) fn heads_are_equivalent(a: &[ChangeHash], b: &[ChangeHash]) -> bool {
    let mut a = a.to_vec();
    let mut b = b.to_vec();
    a.sort();
    b.sort();
    a == b
}

/// Return whether a branch has moved beyond the heads where it was forked.
pub(crate) fn heads_have_changes_since(
    fork_heads: &[ChangeHash],
    current_heads: &[ChangeHash],
) -> bool {
    !heads_are_equivalent(fork_heads, current_heads)
}

/// Represents a location anywhere in Backstitch's history.
/// Associates a branch with heads on that branch.
#[derive(Debug, Clone, Serialize, Deserialize, Reconcile, Hydrate)]
pub struct HistoryRef {
    /// The branch the ref is on.
    #[autosurgeon(with = "crate::helpers::autosurgeon_utils::autosurgeon_doc_id")]
    branch: DocumentId,
    /// The Automerge heads for the history location
    #[autosurgeon(with = "crate::helpers::autosurgeon_utils::autosurgeon_heads")]
    heads: Vec<ChangeHash>,
}

impl HistoryRef {
    pub const BACKSTITCH_SCHEME_PREFIX: &'static str = "backstitch-";
    // these should be safe to use as path seperators; DocumentId is base58-encoded (only a-z, A-Z, 0-9), and ChangeHash is hex-encoded
    pub const BRANCH_DIVIDER: char = '+';
    pub const CHANGE_HASH_DIVIDER: char = '.';

    pub fn new(branch: DocumentId, heads: Vec<ChangeHash>) -> Self {
        // An invariant of HistoryRef is that the heads are always sorted.
        // This ensures that whenever we compare across documents, heads can be relied upon to be ordered.
        let mut heads = heads;
        heads.sort();
        Self { heads, branch }
    }

    pub fn heads(&self) -> &Vec<ChangeHash> {
        &self.heads
    }

    pub fn branch(&self) -> &DocumentId {
        &self.branch
    }

    pub fn is_valid(&self) -> bool {
        !self.heads.is_empty()
    }

    pub fn to_uri_scheme_prefix(&self) -> String {
        format!("{}{}", HistoryRef::BACKSTITCH_SCHEME_PREFIX, self)
    }

    pub fn short_heads(&self) -> String {
        format!(
            "[{}]",
            self.heads
                .iter()
                .map(|h| h.to_string().chars().take(7).collect::<String>())
                .collect::<Vec<String>>()
                .join(", ")
        )
    }
}

impl Eq for HistoryRef {}

impl std::hash::Hash for HistoryRef {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.branch.hash(state);
        self.heads.hash(state);
    }
}

impl PartialEq for HistoryRef {
    fn eq(&self, other: &Self) -> bool {
        self.branch == other.branch && self.heads == other.heads
    }
}

impl Display for HistoryRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if !self.is_valid() {
            return Err(std::fmt::Error);
        }
        let heads_str = self
            .heads
            .iter()
            .map(|h| h.to_string())
            .collect::<Vec<String>>()
            .join(&HistoryRef::CHANGE_HASH_DIVIDER.to_string());
        write!(
            f,
            "{}{}{}",
            self.branch,
            HistoryRef::BRANCH_DIVIDER,
            heads_str
        )
    }
}

impl FromStr for HistoryRef {
    type Err = &'static str;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (doc_id, heads_part) = s
            .split_once(HistoryRef::BRANCH_DIVIDER)
            .ok_or("Invalid history ref string")?;

        let branch =
            DocumentId::from_str(doc_id).map_err(|_| "Invalid DocumentId in history ref string")?;

        let heads = if heads_part.is_empty() {
            Vec::new()
        } else {
            heads_part
                .split(HistoryRef::CHANGE_HASH_DIVIDER)
                .map(|h| ChangeHash::from_str(h).map_err(|_| "Invalid ChangeHash"))
                .collect::<Result<Vec<ChangeHash>, Self::Err>>()?
        };
        let result = HistoryRef { branch, heads };
        if !result.is_valid() {
            return Err("Invalid history ref");
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(byte: u8) -> ChangeHash {
        ChangeHash::from_str(&format!("{byte:02x}").repeat(32)).unwrap()
    }

    #[test]
    fn equivalent_heads_handle_empty_and_single_head_histories() {
        assert!(heads_are_equivalent(&[], &[]));
        assert!(heads_are_equivalent(&[hash(1)], &[hash(1)]));
        assert!(!heads_are_equivalent(&[], &[hash(1)]));
        assert!(!heads_are_equivalent(&[hash(1)], &[hash(2)]));
    }

    #[test]
    fn equivalent_heads_ignore_multi_head_order() {
        assert!(heads_are_equivalent(
            &[hash(1), hash(2), hash(3)],
            &[hash(3), hash(1), hash(2)]
        ));
        assert!(!heads_are_equivalent(
            &[hash(1), hash(2), hash(3)],
            &[hash(1), hash(2), hash(4)]
        ));
    }

    #[test]
    fn branch_changes_are_detected_for_single_and_multi_head_histories() {
        assert!(!heads_have_changes_since(&[hash(1)], &[hash(1)]));
        assert!(heads_have_changes_since(&[hash(1)], &[hash(2)]));
        assert!(!heads_have_changes_since(
            &[hash(1), hash(2)],
            &[hash(2), hash(1)]
        ));
        assert!(heads_have_changes_since(
            &[hash(1), hash(2)],
            &[hash(1), hash(2), hash(3)]
        ));
    }
}
