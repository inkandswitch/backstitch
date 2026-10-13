use godot::classes::resource::DeepDuplicateMode;
use godot::prelude::*;
use godot::{
    classes::{Resource, ResourceLoader, resource_loader::ThreadLoadStatus},
    global,
    obj::Base,
    prelude::GodotClass,
};

#[derive(GodotClass, Debug)]
#[class(base=Resource, tool)]
pub struct LazyLoadToken {
    base: Base<Resource>,
    original_path: Option<String>,
    path: String,
    resource: Option<Gd<Resource>>,
    failed: bool,
}

#[godot_api]
impl IResource for LazyLoadToken {
    fn init(base: Base<Resource>) -> Self {
        Self::create_instance(base, String::new(), None)
    }
}

impl LazyLoadToken {
    fn create_instance(base: Base<Resource>, path: String, original_path: Option<String>) -> Self {
        let mut tok = Self {
            base,
            path,
            original_path,
            resource: None,
            failed: false,
        };
        if !tok.path.is_empty() {
            tok.start_load();
        }
        tok
    }
    pub fn new(path: String, original_path: Option<String>) -> Gd<LazyLoadToken> {
        Gd::from_init_fn(|base| Self::create_instance(base, path, original_path))
    }
}

#[godot_api]
impl LazyLoadToken {
    #[func]
    pub fn is_started(&self) -> bool {
        if self.failed
            || self.resource.is_some() && self.resource.as_ref().unwrap().is_instance_valid()
        {
            return true;
        }
        let status = ResourceLoader::singleton().load_threaded_get_status(&self.path);
        if status != ThreadLoadStatus::INVALID_RESOURCE {
            return true;
        }
        false
    }

    #[func]
    pub fn is_load_finished(&self) -> bool {
        if self.failed
            || self.resource.is_some() && self.resource.as_ref().unwrap().is_instance_valid()
        {
            return true;
        }
        let status = ResourceLoader::singleton().load_threaded_get_status(&self.path);
        if status == ThreadLoadStatus::LOADED || status == ThreadLoadStatus::FAILED {
            return true;
        }
        false
    }

    #[func]
    pub fn start_load(&mut self) {
        if ResourceLoader::singleton().load_threaded_request(&self.path) != global::Error::OK {
            self.failed = true;
        }
    }

    #[func]
    /// DO NOT CALL THIS FROM RUST CODE! IT WILL CAUSE DEADLOCKS!
    /// TODO: need to make the resource loader not have to bind to GodotProject
    pub fn get_resource(&mut self) -> Option<Gd<Resource>> {
        if self.resource.is_some() && self.resource.as_ref().unwrap().is_instance_valid() {
            return self.resource.clone();
        }
        if !self.is_started() {
            self.start_load();
        }
        if self.failed {
            return None;
        }
        let res: Option<Gd<Resource>> = ResourceLoader::singleton().load_threaded_get(&self.path);
        if let Some(mut res) = res
            && res.is_instance_valid()
        {
            if let Some(original_path) = self.original_path.as_ref()
                && &res.get_path().to_string() != original_path
            {
                // Duplicate the resource to avoid caching issues caused by `set_path_cache`.
                // Setting the path cache to another path causes the resource to not be removed from the cache when the resource is freed, leading to a dangling reference.
                // When the resource is loaded again, it will be loaded from the cache, and since the original resource is freed, it will cause a panic when binding to it.
                res = res
                    .duplicate_resource_ex()
                    .deep(DeepDuplicateMode::NONE)
                    .done();
                res.set_path_cache(original_path);
            }
            self.resource = Some(res);
        } else {
            godot_print!("Failed to load resource: {}", self.path);
            self.failed = true;
        }
        self.resource.clone()
    }

    #[func]
    pub fn did_fail(&self) -> bool {
        self.failed
    }

    #[func]
    pub fn get_path(&self) -> GString {
        if let Some(original_path) = self.original_path.as_ref() {
            return GString::from(original_path);
        }
        GString::from(&self.path)
    }
}
