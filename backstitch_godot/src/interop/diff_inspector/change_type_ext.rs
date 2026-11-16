use backstitch::helpers::utils::ChangeType;

/// Presentation helpers for [`ChangeType`] used by the diff inspector.
///
/// These live here (rather than on `ChangeType` itself) because they describe how a change is
/// displayed in the Godot editor, which the core `backstitch` crate shouldn't know about.
pub trait ChangeTypeExt {
    /// The name of the color in the `Editor` theme type used to tint things of this kind.
    fn theme_color_name(&self) -> &'static str;

    /// The suffix appended to the label of a node, sub-resource or property group.
    fn label_suffix(&self) -> &'static str;

    /// The suffix appended to the label of a whole file. Differs from [`Self::label_suffix`] in
    /// that removed files say "Removed" rather than "Deleted".
    fn file_label_suffix(&self) -> &'static str;
}

impl ChangeTypeExt for ChangeType {
    fn theme_color_name(&self) -> &'static str {
        match self {
            ChangeType::Created => "prop_subsection_added",
            ChangeType::Deleted => "prop_subsection_removed",
            ChangeType::Modified => "prop_subsection_modified",
        }
    }

    fn label_suffix(&self) -> &'static str {
        match self {
            ChangeType::Created => "(Added)",
            ChangeType::Deleted => "(Deleted)",
            ChangeType::Modified => "(Modified)",
        }
    }

    fn file_label_suffix(&self) -> &'static str {
        match self {
            ChangeType::Deleted => "(Removed)",
            other => other.label_suffix(),
        }
    }
}
