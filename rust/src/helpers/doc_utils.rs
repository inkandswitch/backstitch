use automerge::{transaction::Transaction, Automerge, ChangeHash, ObjId, Prop, ReadDoc, Value};

#[allow(dead_code)]
pub trait SimpleDocReader {
    fn get_bytes<O: AsRef<ObjId>, P: Into<Prop>>(&self, obj: O, prop: P) -> Option<Vec<u8>>;

    fn get_bytes_at<O: AsRef<ObjId>, P: Into<Prop>>(
        &self,
        obj: O,
        prop: P,
        heads: &[automerge::ChangeHash],
    ) -> Option<Vec<u8>>;

    fn get_int<O: AsRef<ObjId>, P: Into<Prop>>(&self, obj: O, prop: P) -> Option<i64>;

    fn get_int_at<O: AsRef<ObjId>, P: Into<Prop>>(
        &self,
        obj: O,
        prop: P,
        heads: &[automerge::ChangeHash],
    ) -> Option<i64>;

    fn get_float<O: AsRef<ObjId>, P: Into<Prop>>(&self, obj: O, prop: P) -> Option<f64>;

    fn get_string<O: AsRef<ObjId>, P: Into<Prop>>(&self, obj: O, prop: P) -> Option<String>;

    fn get_string_at<O: AsRef<ObjId>, P: Into<Prop>>(
        &self,
        obj: O,
        prop: P,
        heads: &[automerge::ChangeHash],
    ) -> Option<String>;

    fn get_obj_id<O: AsRef<ObjId>, P: Into<Prop>>(&self, obj: O, prop: P) -> Option<ObjId>;

    fn get_obj_id_at<O: AsRef<ObjId>, P: Into<Prop>>(
        &self,
        obj: O,
        prop: P,
        heads: &[automerge::ChangeHash],
    ) -> Option<ObjId>;
}

impl SimpleDocReader for Automerge {
    fn get_bytes<O: AsRef<ObjId>, P: Into<Prop>>(&self, obj: O, prop: P) -> Option<Vec<u8>> {
        match self.get(obj, prop) {
            Ok(Some((Value::Scalar(cow), _))) => match cow.into_owned() {
                automerge::ScalarValue::Bytes(bytes) => Some(bytes),
                _ => None,
            },
            _ => None,
        }
    }

    fn get_bytes_at<O: AsRef<ObjId>, P: Into<Prop>>(
        &self,
        obj: O,
        prop: P,
        heads: &[ChangeHash],
    ) -> Option<Vec<u8>> {
        match self.get_at(obj, prop, heads) {
            Ok(Some((Value::Scalar(cow), _))) => match cow.into_owned() {
                automerge::ScalarValue::Bytes(bytes) => Some(bytes),
                _ => None,
            },
            _ => None,
        }
    }

    fn get_int<O: AsRef<ObjId>, P: Into<Prop>>(&self, obj: O, prop: P) -> Option<i64> {
        match self.get(obj, prop) {
            Ok(Some((Value::Scalar(cow), _))) => match cow.into_owned() {
                automerge::ScalarValue::Int(num) => Some(num),
                _ => None,
            },
            _ => None,
        }
    }

    fn get_int_at<O: AsRef<ObjId>, P: Into<Prop>>(
        &self,
        obj: O,
        prop: P,
        heads: &[automerge::ChangeHash],
    ) -> Option<i64> {
        match self.get_at(obj, prop, &heads) {
            Ok(Some((Value::Scalar(cow), _))) => match cow.into_owned() {
                automerge::ScalarValue::Int(num) => Some(num),
                _ => None,
            },
            _ => None,
        }
    }

    fn get_float<O: AsRef<ObjId>, P: Into<Prop>>(&self, obj: O, prop: P) -> Option<f64> {
        match self.get(obj, prop) {
            Ok(Some((Value::Scalar(cow), _))) => match cow.into_owned() {
                automerge::ScalarValue::F64(num) => Some(num),
                _ => None,
            },
            _ => None,
        }
    }

    fn get_string<O: AsRef<ObjId>, P: Into<Prop>>(&self, obj: O, prop: P) -> Option<String> {
        match self.get(obj, prop) {
            Ok(Some((Value::Scalar(cow), _))) => match cow.into_owned() {
                automerge::ScalarValue::Str(smol_str) => Some(smol_str.to_string()),
                _ => None,
            },
            _ => None,
        }
    }

    fn get_string_at<O: AsRef<ObjId>, P: Into<Prop>>(
        &self,
        obj: O,
        prop: P,
        heads: &[ChangeHash],
    ) -> Option<String> {
        match self.get_at(obj, prop, &heads) {
            Ok(Some((Value::Scalar(cow), _))) => match cow.into_owned() {
                automerge::ScalarValue::Str(smol_str) => Some(smol_str.to_string()),
                _ => None,
            },
            _ => None,
        }
    }

    fn get_obj_id<O: AsRef<ObjId>, P: Into<Prop>>(&self, obj: O, prop: P) -> Option<ObjId> {
        match self.get(obj, prop) {
            Ok(Some((Value::Object(_), obj_id))) => Some(obj_id),
            _ => None,
        }
    }

    fn get_obj_id_at<O: AsRef<ObjId>, P: Into<Prop>>(
        &self,
        obj: O,
        prop: P,
        heads: &[ChangeHash],
    ) -> Option<ObjId> {
        match self.get_at(obj, prop, &heads) {
            Ok(Some((Value::Object(_), obj_id))) => Some(obj_id),
            _ => None,
        }
    }
}

impl SimpleDocReader for Transaction<'_> {
    fn get_bytes<O: AsRef<ObjId>, P: Into<Prop>>(&self, obj: O, prop: P) -> Option<Vec<u8>> {
        match self.get(obj, prop) {
            Ok(Some((Value::Scalar(cow), _))) => match cow.into_owned() {
                automerge::ScalarValue::Bytes(bytes) => Some(bytes),
                _ => None,
            },
            _ => None,
        }
    }

    fn get_bytes_at<O: AsRef<ObjId>, P: Into<Prop>>(
        &self,
        obj: O,
        prop: P,
        heads: &[ChangeHash],
    ) -> Option<Vec<u8>> {
        match self.get_at(obj, prop, heads) {
            Ok(Some((Value::Scalar(cow), _))) => match cow.into_owned() {
                automerge::ScalarValue::Bytes(bytes) => Some(bytes),
                _ => None,
            },
            _ => None,
        }
    }

    fn get_int<O: AsRef<ObjId>, P: Into<Prop>>(&self, obj: O, prop: P) -> Option<i64> {
        match self.get(obj, prop) {
            Ok(Some((Value::Scalar(cow), _))) => match cow.into_owned() {
                automerge::ScalarValue::Int(num) => Some(num),
                _ => None,
            },
            _ => None,
        }
    }

    fn get_int_at<O: AsRef<ObjId>, P: Into<Prop>>(
        &self,
        obj: O,
        prop: P,
        heads: &[automerge::ChangeHash],
    ) -> Option<i64> {
        match self.get_at(obj, prop, &heads) {
            Ok(Some((Value::Scalar(cow), _))) => match cow.into_owned() {
                automerge::ScalarValue::Int(num) => Some(num),
                _ => None,
            },
            _ => None,
        }
    }

    fn get_float<O: AsRef<ObjId>, P: Into<Prop>>(&self, obj: O, prop: P) -> Option<f64> {
        match self.get(obj, prop) {
            Ok(Some((Value::Scalar(cow), _))) => match cow.into_owned() {
                automerge::ScalarValue::F64(num) => Some(num),
                _ => None,
            },
            _ => None,
        }
    }

    fn get_string<O: AsRef<ObjId>, P: Into<Prop>>(&self, obj: O, prop: P) -> Option<String> {
        match self.get(obj, prop) {
            Ok(Some((Value::Scalar(cow), _))) => match cow.into_owned() {
                automerge::ScalarValue::Str(smol_str) => Some(smol_str.to_string()),
                _ => None,
            },
            _ => None,
        }
    }

    fn get_string_at<O: AsRef<ObjId>, P: Into<Prop>>(
        &self,
        obj: O,
        prop: P,
        heads: &[ChangeHash],
    ) -> Option<String> {
        match self.get_at(obj, prop, &heads) {
            Ok(Some((Value::Scalar(cow), _))) => match cow.into_owned() {
                automerge::ScalarValue::Str(smol_str) => Some(smol_str.to_string()),
                _ => None,
            },
            _ => None,
        }
    }

    fn get_obj_id<O: AsRef<ObjId>, P: Into<Prop>>(&self, obj: O, prop: P) -> Option<ObjId> {
        match self.get(obj, prop) {
            Ok(Some((Value::Object(_), obj_id))) => Some(obj_id),
            _ => None,
        }
    }

    fn get_obj_id_at<O: AsRef<ObjId>, P: Into<Prop>>(
        &self,
        obj: O,
        prop: P,
        heads: &[ChangeHash],
    ) -> Option<ObjId> {
        match self.get_at(obj, prop, &heads) {
            Ok(Some((Value::Object(_), obj_id))) => Some(obj_id),
            _ => None,
        }
    }
}
