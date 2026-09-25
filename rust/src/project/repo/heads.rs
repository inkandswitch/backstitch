use std::borrow::Cow;

use automerge::ChangeHash;
use autosurgeon::reconcile::LoadKey;
use autosurgeon::{Hydrate, Prop, ReadDoc, Reconcile, ReconcileError};
use autosurgeon::{Reconciler, hydrate_key};

// todo: should this be nonempty?
#[derive(Debug, Clone, Hash, Default, Hydrate, Reconcile)]
pub struct Heads(
    #[autosurgeon(
        hydrate = "autosurgeon_heads::hydrate",
        reconcile = "autosurgeon_heads::reconcile"
    )]
    Vec<ChangeHash>,
);

impl PartialEq for Heads {
    fn eq(&self, other: &Self) -> bool {
        let mut lhs = self.0.clone();
        let mut rhs = other.0.clone();

        lhs.sort();
        rhs.sort();

        lhs == rhs
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

    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, ChangeHash> {
        self.0.iter_mut()
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
    fn from(value: Vec<ChangeHash>) -> Self {
        Self(value)
    }
}

pub mod autosurgeon_heads {
    use automerge::ChangeHash;
    use autosurgeon::hydrate_key;
    use autosurgeon::reconcile::NoKey;
    use autosurgeon::{Hydrate, HydrateError, Prop, ReadDoc, Reconcile, Reconciler};
    use std::str::FromStr;

    use crate::project::repo::heads::Heads;
    pub fn hydrate<'a, D: ReadDoc>(
        doc: &D,
        obj: &automerge::ObjId,
        prop: Prop<'a>,
    ) -> Result<Vec<ChangeHash>, HydrateError> {
        let inner = Vec::<String>::hydrate(doc, obj, prop)?;
        let heads: Vec<ChangeHash> = inner
            .into_iter()
            .map(|h| {
                ChangeHash::from_str(&h).map_err(|e| {
                    HydrateError::unexpected(
                        "a valid ChangeHash",
                        format!("a ChangeHash which failed to parse due to {}", e),
                    )
                })
            })
            .collect::<Result<_, _>>()?;
        Ok(heads)
    }

    pub fn reconcile<R: Reconciler>(
        heads: &Vec<ChangeHash>,
        reconciler: R,
    ) -> Result<(), R::Error> {
        let str_vec = heads.iter().map(|h| h.to_string()).collect::<Vec<String>>();
        str_vec.reconcile(reconciler)
    }
}
