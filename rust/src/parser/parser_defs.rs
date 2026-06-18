use autosurgeon::{Hydrate, Reconcile};

#[derive(Debug, Clone, Hydrate, Reconcile, PartialEq, Eq)]
pub struct OrderedProperty {
    pub value: String,
    pub order: i64,
}

impl OrderedProperty {
    pub fn get_value(&self) -> String {
        self.value.clone()
    }
}
