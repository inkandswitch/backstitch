use automerge::ChangeHash;

// todo: should this be nonempty?
#[derive(Debug, Clone)]
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

impl From<Vec<ChangeHash>> for Heads {
    fn from(heads: Vec<ChangeHash>) -> Self {
        Self(heads)
    }
}

impl From<Heads> for Vec<ChangeHash> {
    fn from(heads: Heads) -> Self {
        heads.0
    }
}
