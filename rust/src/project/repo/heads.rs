use std::borrow::Cow;

use automerge::ChangeHash;
use autosurgeon::reconcile::{LoadKey, SeqReconciler};
use autosurgeon::{Hydrate, HydrateError, Prop, ReadDoc, Reconcile, ReconcileError, hydrate_prop};
use autosurgeon::{Reconciler, hydrate_key};

// todo: should this be nonempty?
#[derive(Debug, Clone, Hash, Default)]
pub struct Heads(Vec<ChangeHash>);

impl Hydrate for Heads {
    fn hydrate_seq<D: ReadDoc>(
        doc: &D,
        obj: &automerge::ObjId,
    ) -> Result<Self, autosurgeon::HydrateError> {
        let mut heads = Vec::with_capacity(doc.length(obj));

        for idx in 0..doc.length(obj) {
            let bytes: autosurgeon::bytes::ByteVec = hydrate_prop(doc, obj, idx)?;

            let head = ChangeHash::try_from(bytes.as_slice()).map_err(|e| {
                HydrateError::unexpected(
                    "a valid ChangeHash",
                    format!("a ChangeHash which failed to parse due to {}", e),
                )
            })?;

            heads.push(head);
        }

        Ok(Heads::from(heads))
    }
}

impl Reconcile for Heads {
    type Key<'a> = autosurgeon::reconcile::NoKey;

    fn reconcile<R: Reconciler>(&self, mut reconciler: R) -> Result<(), R::Error> {
        let mut seq = reconciler.seq()?;

        // Remove the existing sequence contents.
        while seq.len()? > 0 {
            seq.delete(0)?;
        }

        // Insert the canonical sequence.
        for (idx, head) in self.0.iter().enumerate() {
            seq.insert(
                idx,
                autosurgeon::bytes::ByteVec::from(head.as_ref().to_vec()),
            )?;
        }

        Ok(())
    }
}

impl PartialEq for Heads {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Eq for Heads {}

impl<'a> IntoIterator for &'a Heads {
    type Item = &'a ChangeHash;
    type IntoIter = std::slice::Iter<'a, ChangeHash>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl IntoIterator for Heads {
    type Item = ChangeHash;
    type IntoIter = std::vec::IntoIter<ChangeHash>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl Heads {
    pub fn iter(&self) -> std::slice::Iter<'_, ChangeHash> {
        self.0.iter()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }
}

impl From<Heads> for Vec<ChangeHash> {
    fn from(heads: Heads) -> Self {
        heads.0
    }
}

impl From<Vec<ChangeHash>> for Heads {
    fn from(mut value: Vec<ChangeHash>) -> Self {
        value.sort();
        value.dedup();
        Self(value)
    }
}
