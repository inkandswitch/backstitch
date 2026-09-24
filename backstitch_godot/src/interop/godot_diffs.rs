use std::collections::HashMap;

use backstitch::diff::text_differ::TextDiffLine;
use godot::meta::shape::GodotShape;
use godot::obj::Singleton;
use godot::{
    builtin::{Array, GString, StringName, VarDictionary, Variant, vdict},
    classes::ClassDb,
    global::str_to_var,
    meta::conv::ByValue,
    meta::{ToArg, ToGodot},
};
use regex::Regex;

use crate::interop::{
    godot_helpers::{GodotConvertExt, ToGodotExt, ToVariantExt},
    lazy_load_token::LazyLoadToken,
};
use backstitch::helpers::utils::ChangeType;
use backstitch::{
    diff::{
        differ::{Diff, ProjectDiff},
        resource_differ::BinaryResourceDiff,
        scene_differ::{
            NodeDiff, PropertyDiff, SceneDiff, SubResourceDiff, TextResourceDiff, VariantValue,
        },
        text_differ::{TextDiff, TextDiffHunk},
    },
    parser::godot_parser::TypeOrInstance,
};

use crate::interop::godot_helpers::{
    LocalConvert, LocalTo, LocalToDefaultVariant, LocalToDefaultVariantFn,
};

impl LocalConvert for TextDiffLine {
    type Via = VarDictionary;
    fn godot_shape() -> GodotShape {
        GodotShape::Variant
    }
}

impl LocalTo for TextDiffLine {
    type Pass = ByValue;
    fn to_godot(&self) -> ToArg<'_, Self::Via, Self::Pass> {
        vdict! {
            "new_line_no" => self.new_line_no,
            "old_line_no" => self.old_line_no,
            "content" => &self.content.to_godot(),
            "status" => &self.status.to_godot(),
        }
    }
    fn to_variant(&self) -> Variant {
        self.to_godot().to_variant()
    }
}

impl LocalConvert for TextDiffHunk {
    type Via = VarDictionary;
    fn godot_shape() -> GodotShape {
        GodotShape::Variant
    }
}

impl LocalTo for TextDiffHunk {
    type Pass = ByValue;
    fn to_godot(&self) -> ToArg<'_, Self::Via, Self::Pass> {
        vdict! {
            "new_start" => self.new_start,
            "old_start" => self.old_start,
            "new_lines" => self.new_lines,
            "old_lines" => self.old_lines,
            "diff_lines" => &self.diff_lines.iter().map(|line| line.to_godot()).collect::<Array<VarDictionary>>(),
        }
    }
    fn to_variant(&self) -> Variant {
        self.to_godot().to_variant()
    }
}

impl LocalConvert for TextDiff {
    type Via = VarDictionary;
    fn godot_shape() -> GodotShape {
        GodotShape::Variant
    }
}

impl LocalTo for TextDiff {
    type Pass = ByValue;
    fn to_godot(&self) -> ToArg<'_, Self::Via, Self::Pass> {
        vdict! {
            "path" => &self.path.to_godot(),
            "diff_type" => "text_changed",
            "change_type" => &self.change_type.to_godot(),
            "text_diff" => &vdict! {
                // In the future, if we track renames, we should use the different paths here. Currently we don't, though.
                "new_file" => &self.path.to_godot(),
                "old_file" => &self.path.to_godot(),
                "diff_hunks" => &self.diff_hunks.iter().map(|hunk| hunk.to_godot()).collect::<Array<VarDictionary>>(),
            }
        }
    }
    fn to_variant(&self) -> Variant {
        self.to_godot().to_variant()
    }
}

impl LocalConvert for ChangeType {
    type Via = GString;
    fn godot_shape() -> GodotShape {
        GodotShape::Variant
    }
}

impl LocalToDefaultVariant for ChangeType {
    type Pass = ByValue;
    fn to_godot(&self) -> ToArg<'_, Self::Via, Self::Pass> {
        match self {
            ChangeType::Created => "added",
            ChangeType::Modified => "modified",
            ChangeType::Deleted => "removed",
        }
        .into()
    }
}

impl LocalConvert for Diff {
    type Via = VarDictionary;
    fn godot_shape() -> GodotShape {
        GodotShape::Variant
    }
}

impl LocalToDefaultVariant for Diff {
    type Pass = ByValue;
    fn to_godot(&self) -> ToArg<'_, Self::Via, Self::Pass> {
        match self {
            Diff::Scene(diff) => diff.to_godot(),
            Diff::TextResourceDiff(diff) => diff.to_godot(),
            Diff::BinaryResource(diff) => diff.to_godot(),
            Diff::Text(diff) => diff.to_godot(),
        }
    }
}

impl LocalConvert for ProjectDiff {
    type Via = VarDictionary;
    fn godot_shape() -> GodotShape {
        GodotShape::Variant
    }
}

impl LocalToDefaultVariant for ProjectDiff {
    type Pass = ByValue;
    fn to_godot(&self) -> ToArg<'_, Self::Via, Self::Pass> {
        let mut dict = vdict! {};
        for diff in &self.file_diffs {
            dict.set(
                &match diff {
                    Diff::Scene(scene_diff) => scene_diff.path.clone(),
                    Diff::TextResourceDiff(scene_diff) => scene_diff.path.clone(),
                    Diff::BinaryResource(resource_diff) => resource_diff.path.clone(),
                    Diff::Text(text_diff) => text_diff.path.clone(),
                }
                .to_variant(),
                &diff.to_godot(),
            )
        }
        dict
    }
}

impl LocalConvert for SceneDiff {
    type Via = VarDictionary;
    fn godot_shape() -> GodotShape {
        GodotShape::Variant
    }
}

impl LocalToDefaultVariant for SceneDiff {
    type Pass = ByValue;
    fn to_godot(&self) -> ToArg<'_, Self::Via, Self::Pass> {
        vdict! {
            "change_type" => &self.change_type.to_godot(),
            "changed_nodes" => &self.changed_nodes.to_godot(),
            "diff_type" => "scene_changed"
        }
    }
}

impl LocalConvert for TextResourceDiff {
    type Via = VarDictionary;
    fn godot_shape() -> GodotShape {
        GodotShape::Variant
    }
}

impl LocalToDefaultVariant for TextResourceDiff {
    type Pass = ByValue;
    fn to_godot(&self) -> ToArg<'_, Self::Via, Self::Pass> {
        vdict! {
            "change_type" => &self.change_type.to_godot(),
            "resource_type" => &self.resource_type.to_godot(),
            "changed_sub_resources" => &self.changed_sub_resources.to_godot(),
            "changed_main_resource" => &self.changed_main_resource.as_ref().map(|s| s.to_variant()).unwrap_or(Variant::nil()),
            "diff_type" => "text_resource_changed",
        }
    }
}

impl LocalConvert for SubResourceDiff {
    type Via = VarDictionary;
    fn godot_shape() -> GodotShape {
        GodotShape::Variant
    }
}

impl LocalToDefaultVariant for SubResourceDiff {
    type Pass = ByValue;
    fn to_godot(&self) -> ToArg<'_, Self::Via, Self::Pass> {
        vdict! {
            "change_type" => &self.change_type.to_godot(),
            "sub_resource_id" => &self.sub_resource_id.to_godot(),
            "resource_type" => &self.resource_type.to_godot(),
            "script_class" => &self.script_class.as_ref().map(|s| s.to_godot().to_variant()).unwrap_or(Variant::nil()),
            "changed_props" => &self.changed_properties.to_godot(),
        }
    }
}

impl GodotConvertExt for Vec<SubResourceDiff> {
    type Via = Array<VarDictionary>;
}

impl ToGodotExt for Vec<SubResourceDiff> {
    type Pass = ByValue;
    fn _to_godot(&self) -> Array<VarDictionary> {
        self.iter()
            .map(|s| s.to_godot())
            .collect::<Array<VarDictionary>>()
    }
    fn _to_variant(&self) -> Variant {
        self._to_godot().to_variant()
    }
}

impl GodotConvertExt for Vec<NodeDiff> {
    type Via = Array<VarDictionary>;
}

impl ToGodotExt for Vec<NodeDiff> {
    type Pass = ByValue;
    fn _to_godot(&self) -> Array<VarDictionary> {
        self.iter()
            .map(|s| s.to_godot())
            .collect::<Array<VarDictionary>>()
    }
    fn _to_variant(&self) -> Variant {
        self._to_godot().to_variant()
    }
}

impl LocalConvert for NodeDiff {
    type Via = VarDictionary;
    fn godot_shape() -> GodotShape {
        GodotShape::Variant
    }
}

impl LocalToDefaultVariant for NodeDiff {
    type Pass = ByValue;
    fn to_godot(&self) -> ToArg<'_, Self::Via, Self::Pass> {
        vdict! {
            "change_type" => &self.change_type.to_godot(),
            "changed_props" => &self.changed_properties.to_godot(),
            "node_path" => &self.node_path.to_godot(),
            "type" => &self.node_type.to_variant()
        }
    }
}

impl GodotConvertExt for HashMap<String, PropertyDiff> {
    type Via = VarDictionary;
}

impl ToGodotExt for HashMap<String, PropertyDiff> {
    type Pass = ByValue;
    fn _to_godot(&self) -> VarDictionary {
        let mut dict = vdict! {};
        for (name, diff) in self {
            dict.set(name.clone(), &diff.to_godot());
        }
        dict
    }
    fn _to_variant(&self) -> Variant {
        self._to_godot().to_variant()
    }
}

impl LocalConvert for PropertyDiff {
    type Via = VarDictionary;
    fn godot_shape() -> GodotShape {
        GodotShape::Variant
    }
}

impl LocalConvert for VariantValue {
    type Via = Variant;
    fn godot_shape() -> GodotShape {
        GodotShape::Variant
    }
}

fn get_classdb_default_value(class_name: &str, prop: &str) -> String {
    if ClassDb::singleton().is_instance_valid() && ClassDb::singleton().class_exists(class_name) {
        ClassDb::singleton()
            .class_get_property_default_value(
                &StringName::from(class_name),
                &StringName::from(prop),
            )
            .to_string()
    } else {
        "".to_string()
    }
}

fn str_to_var_safe(s: &str) -> Variant {
    // TODO: This is a temporary fix to avoid errors when Dictionaries and Arrays have embedded resources
    // Once we move to actual variant parsing, we can remove this

    // handle typed arrays and dictionaries that have a script class as the type (e.g. `Array[Resource("foo")]([...])`)
    if (s.starts_with("Array[") || s.starts_with("Dictionary[")) && s.contains("Resource(") {
        let re = Regex::new(r#"(Dictionary|Array)\[[^]]+\]\((.*)\)"#).unwrap();
        let replaced = re
            .captures(s)
            .map(|caps| caps.get(1).map(|m| m.as_str()).unwrap_or(""))
            .unwrap_or("");
        return str_to_var_safe(replaced);
    }
    let re = Regex::new(r#"((?:Sub|Ext)?Resource\([^)]*\))"#).unwrap();
    let replaced = &re
        .replace_all(s, |caps: &regex::Captures| {
            format!(
                "\"{}\"",
                caps.get(0)
                    .map(|m| m.as_str())
                    .unwrap_or("")
                    .replace("\"", "\\\"")
            )
        })
        .to_string();
    str_to_var(replaced)
}

impl LocalToDefaultVariant for VariantValue {
    type Pass = ByValue;
    fn to_godot(&self) -> ToArg<'_, Self::Via, Self::Pass> {
        match self {
            VariantValue::Variant(s) => str_to_var_safe(s),
            VariantValue::DefaultValue(type_or_instance, property_name) => {
                let default_value = match type_or_instance {
                    Some(TypeOrInstance::Type(class_name)) => {
                        get_classdb_default_value(class_name, property_name)
                    }
                    // TODO: we have to get the class of the root instance node; right now this is likely going to be something like `ExtResource("foo")`
                    Some(TypeOrInstance::Instance(_)) => "".to_string(),
                    None => "".to_string(),
                };
                if default_value.is_empty() {
                    "<default_value>".to_string().to_variant()
                } else {
                    str_to_var_safe(&default_value)
                }
            }
            VariantValue::LazyLoadData(original_path, load_path) => {
                LazyLoadToken::new(load_path.clone(), Some(original_path.clone())).to_variant()
            }
            VariantValue::Script(path) => format!("<Script: {}>", path).to_variant(),
        }
    }
}

impl LocalToDefaultVariant for PropertyDiff {
    type Pass = ByValue;
    fn to_godot(&self) -> ToArg<'_, Self::Via, Self::Pass> {
        vdict! {
            "change_type" => &self.change_type.to_godot(),
            "name" => &self.name.to_godot(),
            "new_value" => &self.new_value.as_ref().map(|v| v.to_godot()).unwrap_or(Variant::nil()),
            "old_value" => &self.old_value.as_ref().map(|v| v.to_godot()).unwrap_or(Variant::nil()),
        }
    }
}

impl LocalConvert for BinaryResourceDiff {
    type Via = VarDictionary;
    fn godot_shape() -> GodotShape {
        GodotShape::Variant
    }
}

impl LocalToDefaultVariant for BinaryResourceDiff {
    type Pass = ByValue;
    fn to_godot(&self) -> ToArg<'_, Self::Via, Self::Pass> {
        vdict! {
            "change_type" => &self.change_type.to_godot(),
            "new_resource" => &self.new_resource.as_ref().map(|v| v.to_godot()).unwrap_or(Variant::nil()),
            "old_resource" => &self.old_resource.as_ref().map(|v| v.to_godot()).unwrap_or(Variant::nil()),
            "diff_type" => "resource_changed"
        }
    }
}
