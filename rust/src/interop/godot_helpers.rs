use crate::fs::file_utils::FileContent;
use crate::helpers::utils::ChangedFile;
use crate::parser::godot_parser::TypeOrInstance;
use crate::project::project_api::{BranchViewModel, ChangeViewModel, DiffViewModel, SyncStatus};
use automerge::ChangeHash;
use godot::builtin::Variant;
use godot::meta::conv::{ArgPassing, ByValue};
use godot::meta::shape::GodotShape;
use godot::meta::{GodotType, ToArg};
use godot::{meta::GodotConvert, meta::ToGodot, prelude::*};
use sedimentree_core::id::SedimentreeId;
use std::fmt::Display;
use std::path::PathBuf;

pub trait GodotConvertExt {
    /// The type through which `Self` is represented in Godot.
    type Via: GodotType;
}

pub trait ToGodotExt: Sized + GodotConvertExt {
    /// Target type of [`to_godot()`](ToGodot::to_godot), which can differ from [`Via`][GodotConvert::Via] for pass-by-reference types.
    ///
    /// Note that currently, this only differs from `Via` when `Self` is [`RefArg<'r, T>`][crate::meta::RefArg], which is
    /// used inside generated code of  engine methods. Other uses of `to_godot()`, such as return types in `#[func]`, still use value types.
    /// This may change in future versions.
    ///
    /// See also [`AsArg<T>`](crate::meta::AsArg) used as the "front-end" in Godot API parameters.
    type Pass: ArgPassing;

    /// Converts this type to the Godot type by reference, usually by cloning.
    fn _to_godot(&self) -> ToArg<'_, Self::Via, Self::Pass>;

    fn to_godot(&self) -> ToArg<'_, Self::Via, Self::Pass> {
        self._to_godot()
    }

    /// Converts this type to a [Variant].
    // Exception safety: must not panic apart from exceptional circumstances (Nov 2024: only u64).
    // This has invariant implications, e.g. in Array::resize().
    fn _to_variant(&self) -> Variant;

    fn to_variant(&self) -> Variant {
        self._to_variant()
    }
}

pub trait ToVariantExt: Sized {
    fn _to_variant(&self) -> Variant;
    fn to_variant(&self) -> Variant {
        self._to_variant()
    }
}

impl GodotConvertExt for SedimentreeId {
    type Via = GString;
}

impl ToGodotExt for SedimentreeId {
    type Pass = ByValue;
    fn _to_godot(&self) -> ToArg<'_, Self::Via, Self::Pass> {
        GString::from(&self.to_string())
    }
    fn _to_variant(&self) -> Variant {
        self._to_godot().to_variant()
    }
}

impl ToVariantExt for Option<SedimentreeId> {
    fn _to_variant(&self) -> Variant {
        match self {
            Some(id) => id.to_variant(),
            None => Variant::nil(),
        }
    }
}

impl GodotConvertExt for ChangeHash {
    type Via = GString;
}

impl ToGodotExt for ChangeHash {
    type Pass = ByValue;
    fn _to_godot(&self) -> GString {
        GString::from(&self.to_string())
    }
    fn _to_variant(&self) -> Variant {
        self._to_godot().to_variant()
    }
}

impl ToVariantExt for Option<ChangeHash> {
    fn _to_variant(&self) -> Variant {
        match self {
            Some(id) => id.to_variant(),
            None => Variant::nil(),
        }
    }
}

impl<D: Display> GodotConvertExt for Vec<D> {
    type Via = PackedStringArray;
}

impl<D: Display> ToGodotExt for Vec<D> {
    type Pass = ByValue;
    fn _to_godot(&self) -> ToArg<'_, Self::Via, Self::Pass> {
        self.iter().map(|s| GString::from(&s.to_string())).collect()
    }
    fn _to_variant(&self) -> Variant {
        let thingy = self
            .iter()
            .map(|s| GString::from(&s.to_string()))
            .collect::<PackedStringArray>();
        thingy.to_variant()
    }
}

impl GodotConvertExt for PathBuf {
    type Via = GString;
}

impl ToGodotExt for PathBuf {
    type Pass = ByValue;
    fn _to_godot(&self) -> ToArg<'_, Self::Via, Self::Pass> {
        GString::from(&self.display().to_string())
    }
    fn _to_variant(&self) -> Variant {
        self._to_godot().to_variant()
    }
}

// I couldn't figure out how to use GodotConvert with impls, so just use methods for these.

pub(crate) fn branch_view_model_to_dict(branch: &impl BranchViewModel) -> VarDictionary {
    let merge_into = branch.get_merge_into();
    let var = merge_into.to_variant();
    vdict! {
        "id" => &branch.get_id().to_variant(),
        "name" => branch.get_name(),
        "parent" => &branch.get_parent().to_variant(),
        "children" => &branch.get_children().to_variant(),
        "is_available" => branch.is_available(),
        // todo: figure out how to make to_godot work for this
        "reverted_to" => &branch.get_reverted_to().to_variant(),
        "merge_into" => &var
    }
}

pub(crate) fn diff_view_model_to_dict(diff: &impl DiffViewModel) -> VarDictionary {
    vdict! {
        "dict" => &diff.get_diff().to_godot(),
        "title" => &diff.get_title().to_variant()
    }
}

pub(crate) fn change_view_model_to_dict(change: &impl ChangeViewModel) -> VarDictionary {
    vdict! {
        "hash" => change.get_hash().to_string(),
        "username" => change.get_username(),
        "is_synced" => change.is_synced(),
        "summary" => change.get_summary(),
        "is_merge" => change.is_merge(),
        "merge_id" => &change.get_merge_id().to_variant(),
        "is_setup" => change.is_setup(),
        "exact_timestamp" => change.get_exact_timestamp(),
        "human_timestamp" => change.get_human_timestamp(),
    }
}

impl GodotConvert for SyncStatus {
    type Via = VarDictionary;
    fn godot_shape() -> GodotShape {
        GodotShape::Variant
    }
}

impl ToGodot for SyncStatus {
    type Pass = ByValue;

    fn to_godot(&self) -> VarDictionary {
        vdict! {
            "state" => match self {
                SyncStatus::Unknown => "unknown",
                SyncStatus::Disconnected(_) => "disconnected",
                SyncStatus::UpToDate => "up_to_date",
                SyncStatus::Syncing => "syncing"
            },
            "unsynced_changes" => match self {
                SyncStatus::Disconnected(num) => *num as i32,
                _ => 0
            }
        }
    }
}

impl GodotConvert for FileContent {
    type Via = Variant;
    fn godot_shape() -> GodotShape {
        GodotShape::Variant
    }
}

impl ToGodot for FileContent {
    type Pass = ByValue;
    fn to_godot(&self) -> Variant {
        // < Self as crate::obj::EngineBitfield > ::ord(* self)
        self.to_variant()
    }
    fn to_variant(&self) -> Variant {
        match self {
            FileContent::String(s) => GString::from(s).to_variant(),
            FileContent::Binary(bytes) => PackedByteArray::from(bytes.as_slice()).to_variant(),
            FileContent::Scene(scene) => scene.serialize().to_variant(),
        }
    }
}

impl GodotConvertExt for Vec<ChangedFile> {
    type Via = Array<PackedStringArray>;
}

impl ToGodotExt for Vec<ChangedFile> {
    type Pass = ByValue;
    fn _to_godot(&self) -> Array<PackedStringArray> {
        self.iter()
            .map(|s| {
                let mut inner_array = PackedStringArray::new();
                inner_array.push(&s.path.to_godot());
                inner_array.push(&s.change_type.to_string().to_godot());
                inner_array
            })
            .collect::<Array<PackedStringArray>>()
    }
    fn _to_variant(&self) -> Variant {
        self._to_godot().to_variant()
    }
}

impl GodotConvert for TypeOrInstance {
    type Via = GString;
    fn godot_shape() -> GodotShape {
        GodotShape::Variant
    }
}

impl ToGodot for TypeOrInstance {
    type Pass = ByValue;
    fn to_godot(&self) -> ToArg<'_, Self::Via, Self::Pass> {
        GString::from(&self.to_string())
    }
    fn to_variant(&self) -> Variant {
        self.to_godot().to_variant()
    }
}

impl ToVariantExt for Option<TypeOrInstance> {
    fn _to_variant(&self) -> Variant {
        match self {
            Some(type_or_instance) => type_or_instance.to_variant(),
            None => Variant::nil(),
        }
    }
}
