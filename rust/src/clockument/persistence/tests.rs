use std::{
    collections::{HashMap, HashSet},
    error::Error,
    time::Duration,
};

use async_trait::async_trait;
use automerge::Automerge;
use futures::{StreamExt, stream::BoxStream};
use sedimentree_core::id::SedimentreeId;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;

use crate::clockument::persistence::{DocumentRef, PersistenceTarget};

struct TransientTarget {
    ping: Duration,
    data: HashMap<SedimentreeId, Automerge>,
    stuck: HashSet<DocumentRef>,

    heads_tx: broadcast::Sender<DocumentRef>,
}

impl TransientTarget {}

#[async_trait]
impl PersistenceTarget for TransientTarget {
    async fn put(
        &self,
        id: SedimentreeId,
        doc: Automerge,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        Ok(())
    }

    async fn has(&self, doc_ref: &DocumentRef) -> Result<bool, Box<dyn Error + Send + Sync>> {
        Ok(true)
    }

    async fn get(&self, id: SedimentreeId) -> Result<Automerge, Box<dyn Error + Send + Sync>> {
        Ok(Automerge::new())
    }

    fn new_heads(&self) -> BoxStream<'_, DocumentRef> {
        let str = BroadcastStream::new(self.heads_tx.subscribe());
        let str = str.filter_map(async |r| r.ok());
        str.boxed()
    }
}
