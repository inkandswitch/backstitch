use automerge::ChangeHash;

// todo: should this be nonempty?
#[derive(Debug, Clone, Hash, Default)]
pub struct Heads(Vec<ChangeHash>);

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
