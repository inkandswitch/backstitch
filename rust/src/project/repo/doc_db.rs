use std::{collections::HashMap, sync::Arc};

use automerge::{Automerge, AutomergeError};
use nonempty::NonEmpty;
use sedimentree_core::{
    blob::Blob, fragment::Fragment, id::SedimentreeId, loose_commit::LooseCommit,
};
use thiserror::Error;
use tokio::sync::Mutex;

use crate::project::repo::RepoError;

#[derive(Debug, Clone)]
pub struct DocumentDb {
    docs: Arc<Mutex<HashMap<SedimentreeId, Automerge>>>,
}

#[derive(Error, Debug)]
pub enum DocumentDbError {
    #[error(transparent)]
    Automerge(#[from] AutomergeError),
    #[error("the document was not found: {0}")]
    NoSuchDocument(SedimentreeId),
}

impl DocumentDb {
    pub fn new() -> Self {
        Self {
            docs: Default::default(),
        }
    }

    pub async fn insert_blobs(
        &self,
        id: SedimentreeId,
        mut blobs: Vec<Blob>,
    ) -> Result<(), DocumentDbError> {
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
        doc.load_incremental(&concat)?;
        Ok(())
    }

    pub async fn has(&self, id: &SedimentreeId) -> bool {
        let docs = self.docs.lock().await;
        docs.contains_key(id)
    }

    pub async fn with_document<F, R>(&self, id: &SedimentreeId, f: F) -> Result<R, DocumentDbError>
    where
        F: AsyncFnOnce(&mut Automerge) -> R,
    {
        let mut docs = self.docs.lock().await;
        let doc = docs
            .get_mut(id)
            .ok_or_else(|| DocumentDbError::NoSuchDocument(id.clone()))?;

        let result = f(doc).await;

        Ok(result)
    }

    pub async fn get_fragments(
        &self,
        id: &SedimentreeId,
    ) -> Result<Vec<(automerge::Fragment, Vec<u8>)>, DocumentDbError> {
        let docs = self.docs.lock().await;
        let doc = docs
            .get(id)
            .ok_or_else(|| DocumentDbError::NoSuchDocument(id.clone()))?;

        let frags = doc.fragments(..);
        let bundle = doc.bundle_fragments(frags.clone());

        Ok(frags.into_iter().zip(bundle).collect())
    }
}
