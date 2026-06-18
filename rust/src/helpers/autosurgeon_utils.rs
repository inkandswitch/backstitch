/// Module to hydrate/reconcile a DocumentId from samod.
/// This can be removed once https://github.com/alexjg/samod/issues/58 is addressed.
pub mod autosurgeon_doc_id {
    use autosurgeon::{Hydrate, HydrateError, Prop, ReadDoc, Reconciler};
    use samod::DocumentId;
    use std::str::FromStr;
    pub fn hydrate<'a, D: ReadDoc>(
        doc: &D,
        obj: &automerge::ObjId,
        prop: Prop<'a>,
    ) -> Result<DocumentId, HydrateError> {
        let inner = String::hydrate(doc, obj, prop)?;
        DocumentId::from_str(&inner).map_err(|e| {
            HydrateError::unexpected(
                "a valid DocumentID",
                format!("a DocumentID which failed to parse due to {}", e),
            )
        })
    }

    pub fn reconcile<R: Reconciler>(id: &DocumentId, mut reconciler: R) -> Result<(), R::Error> {
        reconciler.str(id.to_string())
    }
}

/// Module to hydrate/reconcile a Vec<ChangeHash> from automerge.
pub mod autosurgeon_heads {
    use automerge::ChangeHash;
    use autosurgeon::{Hydrate, HydrateError, Prop, ReadDoc, Reconcile, Reconciler};
    use std::str::FromStr;
    pub fn hydrate<'a, D: ReadDoc>(
        doc: &D,
        obj: &automerge::ObjId,
        prop: Prop<'a>,
    ) -> Result<Vec<ChangeHash>, HydrateError> {
        let inner = Vec::<String>::hydrate(doc, obj, prop)?;
        inner
            .into_iter()
            .map(|h| {
                ChangeHash::from_str(&h).map_err(|e| {
                    HydrateError::unexpected(
                        "a valid ChangeHash",
                        format!("a ChangeHash which failed to parse due to {}", e),
                    )
                })
            })
            .collect()
    }

    pub fn reconcile<R: Reconciler>(
        heads: &Vec<ChangeHash>,
        reconciler: R,
    ) -> Result<(), R::Error> {
        let str_vec = heads.iter().map(|h| h.to_string()).collect::<Vec<String>>();
        str_vec.reconcile(reconciler)
    }
}

/// Module to hydrate/reconcile a map of keys DocumentId.
/// This can be removed once https://github.com/alexjg/samod/issues/58 is addressed.
pub mod autosurgeon_branch_map {
    use autosurgeon::{Hydrate, HydrateError, Prop, ReadDoc, Reconcile, Reconciler};
    use samod::DocumentId;
    use std::{collections::HashMap, str::FromStr};

    use crate::helpers::branch::Branch;
    pub fn hydrate<'a, D: ReadDoc>(
        doc: &D,
        obj: &automerge::ObjId,
        prop: Prop<'a>,
    ) -> Result<HashMap<DocumentId, Branch>, HydrateError> {
        let inner = HashMap::<String, Branch>::hydrate(doc, obj, prop)?;
        inner
            .iter()
            .map(|(k, v)| {
                Ok((
                    DocumentId::from_str(k).map_err(|e| {
                        HydrateError::unexpected(
                            "a valid DocumentID",
                            format!("a DocumentID which failed to parse due to {}", e),
                        )
                    })?,
                    v.clone(),
                ))
            })
            .collect()
    }

    pub fn reconcile<R: Reconciler>(
        map: &HashMap<DocumentId, Branch>,
        reconciler: R,
    ) -> Result<(), R::Error> {
        let str_map = map
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect::<HashMap<String, Branch>>();
        str_map.reconcile(reconciler)
    }
}
