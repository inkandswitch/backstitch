use godot::classes::{MissingResource, Object};
use godot::prelude::*;

/// A stand-in object that hosts the (old and new) values of a diff, so that real
/// `EditorProperty` editors can be pointed at it.
///
/// It is a `MissingResource` because those can record arbitrary properties.
/// This is abstracted out primarily so that we can more easily modify
/// the way we use objects in the inspector in the future.
#[derive(Clone)]
pub struct PropertyHost {
    object: Gd<MissingResource>,
}

impl PropertyHost {
    /// Creates a new host. `original_class` is the class the recorded properties conceptually belong to, if known.
    pub fn new(original_class: Option<&str>) -> Self {
        let mut object = MissingResource::new_gd();
        if let Some(class) = original_class {
            object.set_original_class(class);
        }
        Self { object }
    }

    pub fn object(&self) -> Gd<Object> {
        self.object.clone().upcast::<Object>()
    }
}

/// Sets a property on a property host, making sure the host actually records it.
pub fn set_host_property(object: &Gd<Object>, prop_name: &str, prop_value: &Variant) {
    let mut object = object.clone();
    if let Ok(mut fake_object) = object.clone().try_cast::<MissingResource>() {
        fake_object.set_recording_properties(true);
        fake_object.set(prop_name, prop_value);
        fake_object.set_recording_properties(false);
    } else {
        object.set(prop_name, prop_value);
    }
}
