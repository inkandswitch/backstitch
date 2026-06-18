use crate::{
    diff::{differ::Differ, scene_differ::VariantValue},
    fs::file_utils::FileContent,
    helpers::{history_ref::HistoryRef, utils::ChangeType},
};

#[derive(Clone, Debug)]
pub struct BinaryResourceDiff {
    pub path: String,
    pub change_type: ChangeType,
    pub old_resource: Option<VariantValue>,
    pub new_resource: Option<VariantValue>,
}

impl BinaryResourceDiff {
    pub fn new(
        path: String,
        change_type: ChangeType,
        old_resource: Option<VariantValue>,
        new_resource: Option<VariantValue>,
    ) -> BinaryResourceDiff {
        BinaryResourceDiff {
            path,
            change_type,
            old_resource,
            new_resource,
        }
    }
}

impl Differ {
    pub(super) async fn get_binary_resource_diff(
        &self,
        path: &str,
        change_type: ChangeType,
        old_content: Option<&FileContent>,
        new_content: Option<&FileContent>,
        before: &HistoryRef,
        after: &HistoryRef,
    ) -> BinaryResourceDiff {
        BinaryResourceDiff::new(
            path.to_string(),
            change_type,
            self.get_resource(path, old_content, before).await,
            self.get_resource(path, new_content, after).await,
        )
    }

    async fn get_resource(
        &self,
        path: &str,
        _content: Option<&FileContent>,
        ref_: &HistoryRef,
    ) -> Option<VariantValue> {
        if _content.is_none() {
            return None;
        }
        match self.start_load_ext_resource(&path, ref_).await {
            Ok(load_path) => Some(VariantValue::LazyLoadData(path.to_string(), load_path)),
            Err(e) => Some(VariantValue::Variant(format!(
                "\"<ExtResource {} load failed ({})>\"",
                path, e
            ))),
        }
    }
}
