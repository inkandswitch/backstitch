use backstitch::diff::scene_differ::VariantValue;
use godot::builtin::StringName;
use godot::classes::ClassDb;
use godot::obj::Singleton;
use godot::prelude::*;

use super::model::PropertyRow;
use crate::interop::godot_helpers::ToGodotExt;
use backstitch::helpers::utils::ChangeType;

/// What is shown for a value when we can't find out what it is.
const UNKNOWN_DEFAULT: &str = "<default_value>";

fn to_variant(value: &Option<VariantValue>) -> Variant {
    value
        .as_ref()
        .map(|value| value._to_godot())
        .unwrap_or_default()
}

/// Looks up the default value of a property of a class.
///
/// `owner_type` is the class (or, for nodes, the class or an `ExtResource(...)` instance) that has the property;
/// `script_class` is the script class of the owner, if it has one.
pub fn default_value_for_class(
    owner_type: &str,
    prop_name: &str,
    script_class: Option<&str>,
) -> Variant {
    let class_db = ClassDb::singleton();
    let prop_name = StringName::from(prop_name);
    let default_of = |class: &str| {
        let class = StringName::from(class);
        if class_db.class_exists(&class) {
            class_db.class_get_property_default_value(&class, &prop_name)
        } else {
            Variant::nil()
        }
    };

    let mut default = Variant::nil();
    if let Some(script_class) = script_class {
        default = default_of(script_class);
    }
    // We can't get the default value for a script instance
    if default.is_nil()
        && !(owner_type.starts_with("Resource(") || owner_type.starts_with("ExtResource("))
    {
        default = default_of(owner_type);
    }
    if default.is_nil() {
        return UNKNOWN_DEFAULT.to_variant();
    }
    default
}

/// The old and new value of a property row as Godot variants.
///
/// If a value is missing even though the property existed before (or after) the change, it is
/// the property's default value, so it's filled in with that when we know what class the
/// property belongs to (`owner_type`).
pub fn resolve_property_values(
    row: &PropertyRow,
    owner_type: &str,
    script_class: Option<&str>,
) -> (Variant, Variant) {
    let mut old = to_variant(&row.old);
    let mut new = to_variant(&row.new);
    if !owner_type.is_empty() {
        if old.is_nil() && row.kind != ChangeType::Created {
            old = default_value_for_class(owner_type, &row.name, script_class);
        }
        if new.is_nil() && row.kind != ChangeType::Deleted {
            new = default_value_for_class(owner_type, &row.name, script_class);
        }
    }
    (old, new)
}
