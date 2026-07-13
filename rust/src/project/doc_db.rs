use std::{collections::HashMap, sync::Arc};

use automerge::Automerge;
use nonempty::NonEmpty;
use sedimentree_core::{
    blob::Blob, fragment::Fragment, id::SedimentreeId, loose_commit::LooseCommit,
};
use tokio::sync::Mutex;

use crate::project::doc_db::repo::RepoError;

pub mod repo;

#[derive(Clone)]
struct DocumentDb {
    docs: Arc<Mutex<HashMap<SedimentreeId, Automerge>>>,
}

impl DocumentDb {
    pub fn new() -> Self {
        Self {
            docs: Default::default(),
        }
    }

    pub async fn insert_blobs(&mut self, id: SedimentreeId, mut blobs: Vec<Blob>) {
        let mut docs = self.docs.lock().await;
        let entry = docs.entry(id);
        let doc = entry.or_default();
        blobs.sort_by(|a, b| b.contents().len().cmp(&a.contents().len()));
        let concat =
            blobs
                .into_iter()
                .map(|b| b.as_slice().to_vec())
                .fold(Vec::new(), |mut acc, el| {
                    acc.extend(el);
                    acc
                });
        doc.load_incremental(&concat);
    }

    pub(super) async fn with_document<F, R>(&self, id: &SedimentreeId, f: F) -> Result<R, RepoError>
    where
        F: AsyncFnOnce(&mut Automerge) -> R,
    {
        let mut docs = self.docs.lock().await;
        let doc = docs
            .get_mut(id)
            .ok_or_else(|| RepoError::NoSuchDocument(id.clone()))?;

        let result = f(doc).await;

        Ok(result)
    }

    pub(super) async fn get_fragments(
        &self,
        id: &SedimentreeId,
    ) -> Result<Vec<(automerge::Fragment, Vec<u8>)>, RepoError> {
        let docs = self.docs.lock().await;
        let doc = docs
            .get(id)
            .ok_or_else(|| RepoError::NoSuchDocument(id.clone()))?;

        let frags = doc.fragments(..);
        let bundle = doc.bundle_fragments(frags.clone());

        Ok(frags.into_iter().zip(bundle).collect())
    }
}
