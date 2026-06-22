use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use automerge::{Automerge, ChangeHash};
use futures::{Stream, StreamExt};
use samod::{DocHandle, DocumentId};
use tokio::sync::{Mutex, RwLock, watch};
use tokio_stream::wrappers::BroadcastStream;

use crate::{
    helpers::{branch::BranchesMetadataDoc, history_ref::HistoryRef},
    project::branch_db::{BranchDb, CanonicalBranchStatus, ShadowDocWaitError},
};

#[derive(Debug)]
pub(super) struct BranchSyncState {
    pub shadow_doc: Option<Automerge>,
    shadow_doc_init_tx: watch::Sender<bool>,
    pub canonical_doc: DocHandle,
    /// The most up-to-date heads we've seen on the canonical doc
    pub last_tracked: Vec<ChangeHash>,
    /// The last heads on the canonical doc that we reconciled from
    pub last_reconciled: Vec<ChangeHash>,
    /// The binary docs on the canonical doc
    pub canonical_binary_docs: HashMap<DocumentId, BinaryDocStatus>,
    // TODO (Lilith): Figure out a way to reconcile fully synced heads prior to the most recent unsynced heads, if needed.
}

#[derive(PartialEq, Debug)]
pub enum BinaryDocStatus {
    Pending,
    Failed,
    Ok,
}

impl BranchSyncState {
    pub fn new(handle: DocHandle) -> Self {
        Self {
            shadow_doc: None,
            shadow_doc_init_tx: watch::Sender::new(false),
            canonical_doc: handle,
            last_reconciled: Vec::new(),
            last_tracked: Vec::new(),
            canonical_binary_docs: Default::default(),
        }
    }
}

impl BranchDb {
    /// Get the mutable checked out ref for locking.
    /// TODO (Lilith): This smells kind of nasty, maybe don't expose this... but how else to ensure we don't step on toes?
    pub fn get_checked_out_ref_mut(&self) -> Arc<RwLock<Option<HistoryRef>>> {
        self.checked_out_ref.clone()
    }

    pub async fn get_checked_out_ref(&self) -> Option<HistoryRef> {
        return self.checked_out_ref.read().await.clone();
    }

    pub fn subscribe_doc_changes(&self) -> impl Stream<Item = ()> {
        let s = self.branch_change_tx.subscribe();
        BroadcastStream::new(s).filter_map(async |f| f.ok())
    }

    pub async fn get_metadata_state(&self) -> Option<(DocHandle, BranchesMetadataDoc)> {
        // This is a needlessly expensive operation; we should consider allowing reference introspection via external lockers.
        // And/or improve clone perf by reducing string usage in BranchesMetadataDoc.
        self.metadata_state.lock().await.clone()
    }

    pub async fn set_metadata_state(&self, handle: DocHandle, state: BranchesMetadataDoc) {
        let mut st = self.metadata_state.lock().await;
        *st = Some((handle, state));
    }

    pub async fn is_branch_loaded(&self, id: &DocumentId) -> bool {
        let states = self.branch_sync_states.lock().await;
        // branch isn't loaded if we haven't tracked its sync state yet!
        let Some(state) = states.get(id) else {
            return false;
        };
        let state = state.lock().await;
        // if we haven't created a shadow doc, the branch definitely isn't loaded!
        // (I don't think this should ever happen? Consider removing the option.)
        let Some(shadow_doc) = &state.shadow_doc else {
            return false;
        };
        // if the shadow doc has heads, we have at least 1 fully synced commit, which qualifies
        !shadow_doc.get_heads().is_empty()
    }

    pub async fn clear_branch_states(&self) {
        let mut branch_sync_states = self.branch_sync_states.lock().await;
        let mut binary_states = self.binary_states.lock().await;
        branch_sync_states.clear();
        binary_states.clear();
    }

    pub async fn canonical_branch_status(&self, id: &DocumentId) -> CanonicalBranchStatus {
        let states = self.branch_sync_states.lock().await;
        // branch isn't loaded if we haven't tracked its sync state yet!
        let Some(state) = states.get(id) else {
            return CanonicalBranchStatus::BranchNotIngested;
        };
        let state = state.lock().await;

        if state
            .canonical_binary_docs
            .iter()
            .any(|(_, v)| v == &BinaryDocStatus::Failed)
        {
            return CanonicalBranchStatus::BinaryDocNotFound;
        }

        if state
            .canonical_binary_docs
            .iter()
            .any(|(_, v)| v == &BinaryDocStatus::Pending)
        {
            return CanonicalBranchStatus::Pending;
        }

        CanonicalBranchStatus::Healthy
    }

    pub async fn wait_for_shadow_doc(&self, branch: &DocumentId) -> Result<(), ShadowDocWaitError> {
        let mut rx = {
            let states = self.branch_sync_states.lock().await;
            let state = states
                .get(branch)
                .ok_or(ShadowDocWaitError::BranchNotIngested)?;
            let state = state.lock().await;
            state.shadow_doc_init_tx.subscribe()
        };

        let current = *rx.borrow_and_update();
        if current {
            return Ok(());
        }
        rx.changed()
            .await
            .map_err(|_| ShadowDocWaitError::Unknown)?;

        Ok(())
    }

    /// Returns true if a binary doc is fully loaded onto the BranchDb.
    /// This will return true even if the binary doc failed to load... That's so we don't hang forever waiting for nonexistent docs.
    /// But that introduces problems, like server disconnections causing a file checkout! We need to figure out expected failure behavior.
    pub async fn has_binary_doc(&self, id: &DocumentId) -> bool {
        let states = self.binary_states.lock().await;
        states.contains_key(id)
    }

    pub async fn ingest_binary_doc(&self, id: DocumentId, handle: Option<DocHandle>) {
        tracing::debug!("Ingesting binary doc {id}...");
        let mut binary_states = self.binary_states.lock().await;
        if handle.is_none() {
            // If this happens it could trigger a delete... but that's going to have to be OK.
            tracing::error!(
                "Could not fetch binary document {:?}! Notifying waiters anyways.",
                id
            );
        }
        binary_states.insert(id.clone(), handle.clone());

        // check to see if any docs are waiting on this binary doc. If so, remove it from the thing.
        let states = self.branch_sync_states.lock().await;
        for (branch_id, state_arc) in states.iter() {
            let mut state = state_arc.lock().await;

            // if we were waiting on this doc, we may be able to reconcile
            if let Some(status) = state.canonical_binary_docs.get_mut(&id) {
                tracing::debug!(
                    "Ingested binary doc {id} for branch {branch_id}; attempting reconcile"
                );
                *status = match handle {
                    Some(_) => BinaryDocStatus::Ok,
                    None => BinaryDocStatus::Failed,
                };
                drop(state);
                self.try_reconcile_branch(state_arc.clone()).await;
            }
        }
    }

    pub async fn update_branch_sync_state(
        &self,
        handle: DocHandle,
        heads: Vec<ChangeHash>,
        linked_docs: HashSet<DocumentId>,
    ) {
        tracing::debug!("Updating branch sync state...");
        // acquire a lock to our tracked binary states.
        // This prevents anyone from tracking binary docs until we've finished our work.
        let binary_states = self.binary_states.lock().await;

        // add a sync state if it doesn't exist
        let mut states = self.branch_sync_states.lock().await;
        let state_arc = states
            .entry(handle.document_id().clone())
            .or_insert(Arc::new(Mutex::new(BranchSyncState::new(handle))));
        let mut state = state_arc.lock().await;

        // update the linked docs of the sync state
        state.canonical_binary_docs = linked_docs
            .into_iter()
            .map(|id| match binary_states.get(&id) {
                Some(Some(_)) => (id, BinaryDocStatus::Ok),
                Some(None) => (id, BinaryDocStatus::Failed),
                None => (id, BinaryDocStatus::Pending),
            })
            .collect();
        state.last_tracked = heads;

        // if we're already synced, we can definitely reconcile
        if Self::resolved_all_canonical_binary_docs(&state.canonical_binary_docs) {
            // no double lock allowed!
            drop(state);
            self.try_reconcile_branch(state_arc.clone()).await;
        }

        let _ = self.branch_change_tx.send(());

        // Now that we release the lock to binary_states here, whenever someone else uses ingest_binary_doc(), it will look at our states
        // and remove stuff from waiting_binary_docs when it syncs.
    }

    // we may need to do an unordered comparison for heads across docs
    // todo: we may want to factor this out to a better Heads struct to handle correct comparison always
    fn are_heads_equivalent(a: &[ChangeHash], b: &[ChangeHash]) -> bool {
        let mut asorted = a.to_vec();
        let mut bsorted = b.to_vec();
        asorted.sort();
        bsorted.sort();
        asorted == bsorted
    }

    fn resolved_all_canonical_binary_docs(
        binary_docs: &HashMap<DocumentId, BinaryDocStatus>,
    ) -> bool {
        binary_docs
            .iter()
            .all(|(_, status)| *status != BinaryDocStatus::Pending)
    }

    pub(super) async fn try_reconcile_branch(&self, sync_state: Arc<Mutex<BranchSyncState>>) {
        let doc_change_tx = self.branch_change_tx.clone();
        tokio::task::spawn_blocking(move || {
            // this is quite weird, but we want to be holding the state mutex this entire method.
            let mut state = sync_state.blocking_lock();

            if !Self::resolved_all_canonical_binary_docs(&state.canonical_binary_docs) {
                tracing::debug!("Could not reconcile because we're still waiting on binary docs.");
                return;
            }

            // did we track any new changes coming into the canonical?
            if Self::are_heads_equivalent(&state.last_reconciled, &state.last_tracked) {
                // is canonical still synced up with the shadow doc?
                if let Some(shadow_doc) = &state.shadow_doc
                    && Self::are_heads_equivalent(&state.last_reconciled, &shadow_doc.get_heads())
                {
                    // if both of those were true, we don't actually need to reconcile.
                    tracing::debug!("Could not reconcile because we're already up-to-date.");
                    return;
                }
            }

            tracing::debug!("Reconcile starting...");

            // let tracked_heads = state.last_tracked.clone();
            let handle = state.canonical_doc.clone();

            let (mut state, new_heads) = handle.with_document(move |d| {
                // First, create a fork from our heads if we don't have one
                let shadow_doc = state
                    .shadow_doc
                    // TODO (Lilith): Once Alex fixes fork_at, use the other line instead
                    // .get_or_insert_with(|| d.fork_at(&tracked_heads).unwrap());
                    .get_or_insert_with(|| d.fork());

                // First, fork at tracked heads.
                // This is important so that if new heads have appeared with unsynced binary docs since
                // we tried to reconcile, we don't include them.

                // // TODO (Lilith): Once Alex fixes fork_at, use this code instead of merging directly...
                // let mut fork = d.fork_at(&tracked_heads).unwrap();

                // // Next, sync our fork with the shadow doc.
                // let _ = fork.merge(shadow_doc).unwrap();
                // let _ = shadow_doc.merge(&mut fork).unwrap();

                let _ = shadow_doc.merge(d).unwrap();

                // Last, sync our canonical doc with the shadow doc.
                // We need to ignore the outputted heads, because we may already have unsynced changes in the canonical doc!
                // document_watcher will pick up on any meaningful changes here, and will handle ingestion for us.
                let _ = d.merge(shadow_doc).unwrap();
                (state, d.get_heads())
            });
            // TODO (Lilith): Figure out a way to ignore canonical heads (use shadow heads?)
            state.last_reconciled = new_heads.clone();
            state.last_tracked = new_heads;
            tracing::debug!("Reconcile completed.");
            let _ = state.shadow_doc_init_tx.send_replace(true);
            let _ = doc_change_tx.send(());
        })
        .await
        .unwrap();
    }
}
