use automerge::AutomergeError;
use autosurgeon::{
    Hydrate, Reconcile,
    hydrate::Unexpected,
    hydrate_key,
    reconcile::{LoadKey, MapReconciler, NoKey},
};
use sedimentree_core::id::SedimentreeId;

use crate::project::repo::heads::Heads;

// newtype this just to implement hydrate/reconcile
#[derive(Clone, Debug, Copy, PartialEq, Eq, Hash)]
struct DocumentId(SedimentreeId);

impl Hydrate for DocumentId {
    fn hydrate_bytes(bytes: &[u8]) -> Result<Self, autosurgeon::HydrateError> {
        Ok(DocumentId(SedimentreeId::from_bytes(
            bytes.try_into().map_err(|_| {
                autosurgeon::HydrateError::Unexpected(Unexpected::Other {
                    expected: "a 32-byte sequence".to_string(),
                    found: "something else".to_string(),
                })
            })?,
        )))
    }
}

impl Reconcile for DocumentId {
    type Key<'a> = NoKey;

    fn reconcile<R: autosurgeon::Reconciler>(&self, mut reconciler: R) -> Result<(), R::Error> {
        reconciler.bytes(self.0.as_bytes())?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Hydrate)]
pub struct DocumentRef {
    id: DocumentId,
    heads: Heads,
}

impl DocumentRef {
    pub fn new(id: SedimentreeId, heads: Heads) -> Self {
        Self {
            id: DocumentId(id),
            heads,
        }
    }

    pub fn id(&self) -> SedimentreeId {
        self.id.0
    }

    pub fn heads(&self) -> &Heads {
        &self.heads
    }
}

// Need to manually implement Reconcile because we want to keep DocumentId wrapper private
impl Reconcile for DocumentRef {
    type Key<'a> = SedimentreeId;

    fn reconcile<R: autosurgeon::Reconciler>(&self, mut reconciler: R) -> Result<(), R::Error> {
        let mut m = reconciler.map()?;
        m.put("id", &self.id)?;
        m.put("heads", &self.heads)?;

        Ok(())
    }

    fn hydrate_key<'a, D: autosurgeon::ReadDoc>(
        doc: &D,
        obj: &automerge::ObjId,
        prop: autosurgeon::Prop<'_>,
    ) -> Result<LoadKey<Self::Key<'a>>, autosurgeon::ReconcileError> {
        let key: LoadKey<DocumentId> = hydrate_key(doc, obj, prop, "id".into())?;
        Ok(key.map(|k| k.0))
    }

    fn key(&self) -> LoadKey<Self::Key<'_>> {
        LoadKey::Found(self.id.0)
    }
}
