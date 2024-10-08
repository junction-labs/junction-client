use std::borrow::Cow;
use std::collections::BTreeMap;
use std::str::FromStr;

use crate::backend::{Backend, LbPolicy};
use crate::error::{path_from_str, path_str, Error, PathEntry};
use crate::shared::{ServiceTarget, Target};

use serde::{Deserialize, Serialize};
use serde_json::Map;
use serde_json::Value;

impl Backend {
    /// Write this [Backend] config as flattened K/V pairs, into an
    /// existing map of Kube annotations.
    ///
    /// Annotation keys are always prefixed with `junctionlabs.io`.
    pub fn as_annotations(&self, annotations: &mut BTreeMap<String, String>) -> Result<(), Error> {
        let as_json = serde_json::to_value(PartialBackend::borrow(self)).map_err(|_| {
            Error::new_static("failed to serialize Backend. this is a bug, please file an issue")
        })?;
        flatten_value(Some("junctionlabs.io"), &as_json, annotations)
    }

    /// Parse a [Backend] from flattened K/V pairs.
    ///
    /// Expects annotation keys to be prefixed with `junctionlabs.io`.
    pub fn from_annotations(
        namespace: &str,
        name: &str,
        port: Option<u16>,
        annotations: &BTreeMap<String, String>,
    ) -> Result<Self, Error> {
        let value = unflatten_value(annotations.iter(), Some("junctionlabs.io"))?;

        let partial: PartialBackend = serde_json::from_value(value)
            .map_err(|e| Error::new(format!("failed to deserialize lb: {e}")).with_field("lb"))?;

        let backend = partial.into_backend(Target::Service(ServiceTarget {
            name: name.to_string(),
            namespace: namespace.to_string(),
            port,
        }));

        Ok(backend)
    }
}

/// The parts of a [Backend] that should be serialized to/from annotations.
#[derive(Debug, Default, Serialize, Deserialize)]
struct PartialBackend<'a> {
    #[serde(default)]
    lb: Cow<'a, LbPolicy>,
}

impl<'a> PartialBackend<'a> {
    fn borrow(backend: &'a Backend) -> Self {
        Self {
            lb: Cow::Borrowed(&backend.lb),
        }
    }

    fn into_backend(self, target: Target) -> Backend {
        Backend {
            target,
            lb: self.lb.into_owned(),
        }
    }
}

fn to_annotation_value(value: &Value) -> String {
    if let Some(str) = value.as_str() {
        str.to_string()
    } else {
        value.to_string()
    }
}

fn from_annotation_value(s: &str) -> Value {
    match Value::from_str(s) {
        Ok(v) => v,
        Err(_) => Value::String(s.to_string()),
    }
}

fn flatten_value(
    prefix: Option<&'static str>,
    value: &Value,
    dst: &mut BTreeMap<String, String>,
) -> Result<(), Error> {
    struct Visitor<'a> {
        prefix: Option<&'static str>,
        dst: &'a mut BTreeMap<String, String>,
        path: Vec<PathEntry>,
    }

    impl<'a> Visitor<'a> {
        fn visit_any(&mut self, v: &Value) -> Result<(), Error> {
            match v {
                Value::Array(items) => self.visit_array(items)?,
                Value::Object(m) => self.visit_map(m)?,
                value => {
                    let path = path_str(self.prefix, self.path.iter());
                    self.dst.insert(path, to_annotation_value(value));
                }
            }

            Ok(())
        }

        fn visit_map(&mut self, map: &Map<String, Value>) -> Result<(), Error> {
            for (key, v) in map.iter() {
                self.path.push(PathEntry::from(key.clone()));

                match v {
                    Value::Array(items) => self.visit_array(items)?,
                    Value::Object(m) => self.visit_map(m)?,
                    value => self.visit_any(value)?,
                }

                self.path.pop();
            }

            Ok(())
        }

        fn visit_array(&mut self, array: &[Value]) -> Result<(), Error> {
            for (i, e) in array.iter().enumerate() {
                self.path.push(PathEntry::Index(i));
                self.visit_any(e)?;
                self.path.pop();
            }

            Ok(())
        }
    }

    match value {
        Value::Object(map) => {
            let mut v = Visitor {
                dst,
                prefix,
                path: vec![],
            };
            v.visit_map(map)
        }
        _ => Err(Error::new_static("only objects can be flattened")),
    }
}

fn unflatten_value<'a>(
    kvs: impl Iterator<Item = (&'a String, &'a String)>,
    prefix: Option<&'static str>,
) -> Result<Value, Error> {
    let mut m = Map::new();

    for (k, v) in kvs {
        if prefix.map_or(true, |p| k.starts_with(p)) {
            let mut path = path_from_str(k)?;
            path.reverse();

            match path.pop() {
                Some(PathEntry::Field(field)) => {
                    let value = from_annotation_value(v);
                    unflatten_object_field(&mut m, path, field.to_string(), value)?
                }
                Some(PathEntry::Index(_)) => {
                    return Err(Error::new_static("can't unflatten a top-level array"))
                }
                None => return Err(Error::new_static("empty key path")),
            }
        }
    }

    Ok(Value::Object(m))
}

fn unflatten_object_field(
    m: &mut Map<String, Value>,
    mut key_path: Vec<PathEntry>,
    field: String,
    value: Value,
) -> Result<(), Error> {
    match key_path.pop() {
        Some(PathEntry::Field(next_field)) => {
            let entry = m.entry(field).or_insert_with(|| Value::Object(Map::new()));
            let nested = entry
                .as_object_mut()
                .ok_or_else(|| Error::new_static("can't set properties on a non-map field"))?;

            unflatten_object_field(nested, key_path, next_field.to_string(), value)
        }
        Some(PathEntry::Index(idx)) => {
            let entry = m.entry(field).or_insert_with(|| Value::Array(Vec::new()));
            let nested = entry.as_array_mut().ok_or_else(|| {
                // TODO: it'd be nice to put the value in this message, but
                // borrowck is mean
                Error::new_static("can't set elements on a non-array field")
            })?;

            unflatten_array_field(nested, key_path, idx, value)
        }
        None => {
            m.insert(field, value);
            Ok(())
        }
    }
}

fn unflatten_array_field(
    v: &mut Vec<Value>,
    mut key_path: Vec<PathEntry>,
    idx: usize,
    value: Value,
) -> Result<(), Error> {
    if v.len() <= idx {
        v.resize(idx + 1, Value::Null);
    }

    match key_path.pop() {
        Some(PathEntry::Field(field)) => {
            let m = match &mut v[idx] {
                v @ Value::Null => {
                    *v = Value::Object(Map::new());
                    v.as_object_mut().unwrap()
                }
                Value::Object(map) => map,
                _ => {
                    return Err(Error::new(format!(
                        "can't set properties on non-map array element: '{}'",
                        v[idx]
                    )))
                }
            };

            unflatten_object_field(m, key_path, field.to_string(), value)
        }
        Some(PathEntry::Index(nested_idx)) => {
            let nested = match &mut v[idx] {
                v @ Value::Null => {
                    *v = Value::Array(Vec::new());
                    v.as_array_mut().unwrap()
                }
                Value::Array(nested) => nested,
                _ => {
                    return Err(Error::new(format!(
                        "can't set elements on non-array array element: '{}'",
                        v[idx],
                    )))
                }
            };

            unflatten_array_field(nested, key_path, nested_idx, value)
        }
        None => {
            v[idx] = value;
            Ok(())
        }
    }
}

#[cfg(test)]
mod test {
    use crate::{backend::LbPolicy, shared::ServiceTarget};

    use super::*;

    #[test]
    fn test_backend_empty_annotations() {
        let annotations = BTreeMap::new();
        let default_backend = Backend::from_annotations("foo", "bar", None, &annotations).unwrap();

        assert_eq!(
            default_backend,
            Backend {
                target: Target::Service(ServiceTarget {
                    name: "bar".to_string(),
                    namespace: "foo".to_string(),
                    port: None
                }),
                lb: LbPolicy::Unspecified,
            }
        )
    }

    #[test]
    fn test_backend_roundtrips() {
        let web = ServiceTarget {
            name: "web".to_string(),
            namespace: "prod".to_string(),
            port: None,
        };

        let backend = Backend {
            target: Target::Service(web.clone()),
            lb: crate::backend::LbPolicy::RingHash(crate::backend::RingHashParams {
                min_ring_size: 1024,
                hash_params: vec![
                    crate::shared::SessionAffinityHashParam {
                        terminal: false,
                        matcher: crate::shared::SessionAffinityHashParamType::Header {
                            name: "x-user".to_string(),
                        },
                    },
                    crate::shared::SessionAffinityHashParam {
                        terminal: false,
                        matcher: crate::shared::SessionAffinityHashParamType::Header {
                            name: "x-env".to_string(),
                        },
                    },
                ],
            }),
        };

        assert_eq!(backend, {
            let mut annotations = BTreeMap::new();
            backend.as_annotations(&mut annotations).unwrap();
            Backend::from_annotations(&web.namespace, &web.name, web.port, &annotations).unwrap()
        });
    }

    #[test]
    fn test_flatten_unflatten() {
        let value = serde_json::json!({
            "foo": ["hi", "potato"],
            "bar": {
                "baz": {
                    "one": 1,
                    "three": [1, 2, 3],
                    "two": "two",
                },
                "hi": false,
            },
            "arrays": [
                ["one", 1],
                [2, "two"],
            ],
        });
        let expected: BTreeMap<String, String> = BTreeMap::from_iter(
            [
                ("arrays[0][0]", "one"),
                ("arrays[0][1]", "1"),
                ("arrays[1][0]", "2"),
                ("arrays[1][1]", "two"),
                ("bar.baz.one", "1"),
                ("bar.baz.three[0]", "1"),
                ("bar.baz.three[1]", "2"),
                ("bar.baz.three[2]", "3"),
                ("bar.baz.two", "two"),
                ("bar.hi", "false"),
                ("foo[0]", "hi"),
                ("foo[1]", "potato"),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string())),
        );

        let mut flattened = BTreeMap::new();
        flatten_value(None, &value, &mut flattened).unwrap();

        assert_eq!(flattened, expected);
        assert_eq!(unflatten_value(flattened.iter(), None).unwrap(), value);
    }

    #[test]
    fn test_flatten_unflatten_prefix() {
        let value = serde_json::json!({
            "foo": ["hi", "potato"],
            "bar": {
                "baz": {
                    "one": 1,
                    "three": [1, 2, 3],
                    "two": "two",
                },
                "hi": false,
            },
            "arrays": [
                ["one", 1],
                [2, "two"],
            ],
        });
        let expected: BTreeMap<String, String> = BTreeMap::from_iter(
            [
                ("prefix/arrays[0][0]", "one"),
                ("prefix/arrays[0][1]", "1"),
                ("prefix/arrays[1][0]", "2"),
                ("prefix/arrays[1][1]", "two"),
                ("prefix/bar.baz.one", "1"),
                ("prefix/bar.baz.three[0]", "1"),
                ("prefix/bar.baz.three[1]", "2"),
                ("prefix/bar.baz.three[2]", "3"),
                ("prefix/bar.baz.two", "two"),
                ("prefix/bar.hi", "false"),
                ("prefix/foo[0]", "hi"),
                ("prefix/foo[1]", "potato"),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string())),
        );

        let mut flattened = BTreeMap::new();
        flatten_value(Some("prefix"), &value, &mut flattened).unwrap();

        assert_eq!(flattened, expected);
        assert_eq!(
            unflatten_value(flattened.iter(), Some("prefix")).unwrap(),
            value
        );
    }
}
