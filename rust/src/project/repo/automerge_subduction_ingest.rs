//! CLI tool for ingesting Automerge documents into Subduction.
//!
//! Reads an `.am` file, decomposes it into sedimentree fragments and loose
//! commits, connects to a Subduction sync server via WebSocket, and uploads
//! the data.
//!
//! # Usage
//!
//! ```sh
//! automerge_subduction_ingest \
//!   --server wss://sync.example.com \
//!   --ephemeral-key \
//!   document.am
//! ```

use std::{collections::BTreeSet, path::PathBuf, sync::Arc};

use automerge::Automerge;
use future_form::Sendable;
use sedimentree_core::{
    blob::{Blob, BlobMeta},
    collections::Set,
    fragment::Fragment,
    id::SedimentreeId,
    loose_commit::{LooseCommit, id::CommitId},
    sedimentree::Sedimentree,
};
use subduction_core::{
    handshake::audience::Audience, policy::open::OpenPolicy, storage::memory::MemoryStorage,
    subduction::builder::SubductionBuilder, timeout::call::CallTimeout,
    transport::message::MessageTransport,
};
use subduction_crypto::signer::memory::MemorySigner;
use subduction_websocket::tokio::{TimeoutTokio, TokioSpawn, client::TokioWebSocketClient};

/// Result of ingesting an Automerge document.
pub struct IngestResult {
    pub sedimentree: Sedimentree,
    pub blobs: Vec<Blob>,
    pub change_count: usize,
    pub covered_count: usize,
    pub loose_count: usize,
    pub fragment_count: usize,
}

/// Ingest an Automerge document into a [`Sedimentree`].
///
/// Thin adapter over [`Automerge::fragments`] and
/// [`Automerge::bundle_fragments`]: maps each level-1+ `automerge::Fragment`
/// into a sedimentree [`Fragment`] and each level-0 fragment into a
/// [`LooseCommit`].
pub fn ingest_automerge(doc: &Automerge, sedimentree_id: SedimentreeId) -> IngestResult {
    let cached = doc.fragments(1..);
    let loose = doc.fragments(0..=0);
    let cached_bytes = doc.bundle_fragments(cached.iter().cloned());
    let loose_bytes = doc.bundle_fragments(loose.iter().cloned());

    let mut fragments = Vec::with_capacity(cached.len());
    let mut blobs = Vec::with_capacity(cached.len() + loose.len());
    let mut covered: Set<CommitId> = Set::new();
    for (f, raw) in cached.iter().zip(cached_bytes) {
        for m in &f.members {
            covered.insert(CommitId::new(m.0));
        }
        let blob = Blob::new(raw);
        let boundary: BTreeSet<CommitId> = f.boundary.iter().map(|h| CommitId::new(h.0)).collect();
        let checkpoints: Vec<CommitId> = f.checkpoints.iter().map(|h| CommitId::new(h.0)).collect();
        let meta = BlobMeta::new(&blob);
        fragments.push(Fragment::new(
            sedimentree_id,
            CommitId::new(f.head.0),
            boundary,
            &checkpoints,
            meta,
        ));
        blobs.push(blob);
    }

    let mut loose_commits = Vec::with_capacity(loose.len());
    for (f, raw) in loose.iter().zip(loose_bytes) {
        let head = CommitId::new(f.head.0);
        let parents: BTreeSet<CommitId> = f.boundary.iter().map(|p| CommitId::new(p.0)).collect();
        let blob = Blob::new(raw);
        let meta = BlobMeta::new(&blob);
        loose_commits.push(LooseCommit::new(sedimentree_id, head, parents, meta));
        blobs.push(blob);
    }

    let fragment_count = fragments.len();
    let loose_count = loose_commits.len();
    let covered_count = covered.len();

    IngestResult {
        sedimentree: Sedimentree::new(fragments, loose_commits),
        blobs,
        change_count: doc.get_changes_meta(&[]).len(),
        covered_count,
        loose_count,
        fragment_count,
    }
}
