//! NSKeyedArchive objects required by the IDE protocol (not plain dictionaries).
use crate::Result;
use plist::{Dictionary, Uid, Value};

fn dict(items: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    Value::Dictionary(items.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}
fn class(name: &str) -> Value {
    dict([
        ("$classname", name.into()),
        ("$classes", Value::Array(vec![name.into()])),
    ])
}
fn uid(n: u64) -> Value {
    Value::Uid(Uid::new(n))
}
fn finish(objects: Vec<Value>) -> Result<Vec<u8>> {
    let archive = dict([
        ("$archiver", "NSKeyedArchiver".into()),
        ("$version", 100000u64.into()),
        ("$top", dict([("root", uid(1))])),
        ("$objects", Value::Array(objects)),
    ]);
    let mut bytes = Vec::new();
    archive.to_writer_binary(&mut bytes)?;
    Ok(bytes)
}
pub(crate) fn nsuuid(id: uuid::Uuid) -> Result<Vec<u8>> {
    finish(vec![
        "$null".into(),
        dict([
            ("$class", uid(2)),
            ("NS.uuidbytes", Value::Data(id.as_bytes().to_vec())),
        ]),
        class("NSUUID"),
    ])
}
pub(crate) fn capabilities(caps: Dictionary) -> Result<Vec<u8>> {
    let mut objects = vec![
        "$null".into(),
        dict([("$class", uid(2)), ("capabilities-dictionary", uid(3))]),
        class("XCTCapabilities"),
        Value::Boolean(false),
        class("NSDictionary"),
    ];
    let mut keys = Vec::new();
    let mut values = Vec::new();
    for (key, value) in caps {
        keys.push(uid(objects.len() as u64));
        objects.push(key.into());
        values.push(uid(objects.len() as u64));
        objects.push(value);
    }
    objects[3] = dict([
        ("$class", uid(4)),
        ("NS.keys", Value::Array(keys)),
        ("NS.objects", Value::Array(values)),
    ]);
    finish(objects)
}
/// Resolve custom archived objects too: upstream leaves XCTCapabilities UIDs intact.
pub(crate) fn readable_reply(
    message: &idevice::dvt::message::Message,
) -> Result<serde_json::Value> {
    let Some(bytes) = &message.raw_data else {
        return Ok(serde_json::Value::Null);
    };
    decode_json(bytes)
}
pub(crate) fn decode_json(bytes: &[u8]) -> Result<serde_json::Value> {
    let archive = Value::from_reader(std::io::Cursor::new(bytes))?;
    let dictionary = archive
        .as_dictionary()
        .ok_or_else(|| crate::Error::Protocol("archive is not a dictionary".into()))?;
    let objects = dictionary
        .get("$objects")
        .and_then(Value::as_array)
        .ok_or_else(|| crate::Error::Protocol("archive missing $objects".into()))?;
    let root = dictionary
        .get("$top")
        .and_then(Value::as_dictionary)
        .and_then(|top| top.get("root"))
        .ok_or_else(|| crate::Error::Protocol("archive missing root".into()))?;
    resolve(root, objects, &mut Vec::new())
}
fn resolve(value: &Value, objects: &[Value], stack: &mut Vec<usize>) -> Result<serde_json::Value> {
    use serde_json::{Value as Json, json};
    let invalid = |detail: &str| crate::Error::Protocol(format!("NSKeyedArchive: {detail}"));
    if stack.len() > 128 {
        return Err(invalid("nesting limit exceeded"));
    }
    Ok(match value {
        Value::Uid(id) => {
            let index = usize::try_from(id.get()).map_err(|_| invalid("UID overflow"))?;
            if stack.contains(&index) {
                return Err(invalid("cyclic UID"));
            }
            let object = objects
                .get(index)
                .ok_or_else(|| invalid("UID out of bounds"))?;
            stack.push(index);
            let result = resolve(object, objects, stack);
            stack.pop();
            result?
        }
        Value::Dictionary(d) => {
            if let Some(keys) = d.get("NS.keys").and_then(Value::as_array) {
                let values = d
                    .get("NS.objects")
                    .and_then(Value::as_array)
                    .ok_or_else(|| invalid("dictionary missing values"))?;
                if keys.len() != values.len() {
                    return Err(invalid("dictionary length mismatch"));
                }
                let mut result = serde_json::Map::new();
                for (key, value) in keys.iter().zip(values) {
                    let key = resolve(key, objects, stack)?;
                    let key = key
                        .as_str()
                        .ok_or_else(|| invalid("non-string dictionary key"))?;
                    result.insert(key.into(), resolve(value, objects, stack)?);
                }
                Json::Object(result)
            } else {
                let mut result = serde_json::Map::new();
                for (key, value) in d {
                    if key == "$class" {
                        continue;
                    }
                    result.insert(key.clone(), resolve(value, objects, stack)?);
                }
                Json::Object(result)
            }
        }
        Value::Array(values) => Json::Array(
            values
                .iter()
                .map(|v| resolve(v, objects, stack))
                .collect::<Result<_>>()?,
        ),
        Value::String(s) if s == "$null" => Json::Null,
        Value::String(s) => json!(s),
        Value::Boolean(b) => json!(b),
        Value::Integer(i) => {
            if let Some(n) = i.as_signed() {
                json!(n)
            } else {
                json!(i.as_unsigned())
            }
        }
        Value::Real(n) => json!(n),
        Value::Data(bytes) => json!({"bytes": bytes}),
        Value::Date(date) => json!(date.to_xml_format()),
        _ => return Err(invalid("unsupported plist value")),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn custom_capabilities_are_resolved_to_json() {
        let caps: Dictionary = [
            ("supports-value".to_owned(), Value::Boolean(true)),
            ("version".to_owned(), Value::Integer(36u64.into())),
        ]
        .into_iter()
        .collect();
        let decoded = decode_json(&capabilities(caps).unwrap()).unwrap();
        assert_eq!(
            decoded,
            serde_json::json!({"capabilities-dictionary": {"supports-value": true, "version": 36}})
        );
    }
    #[test]
    fn invalid_and_cyclic_references_are_rejected() {
        assert!(resolve(&uid(9), &[], &mut vec![]).is_err());
        assert!(resolve(&uid(0), &[uid(0)], &mut vec![]).is_err());
    }
    #[test]
    fn uuid_is_typed_and_preserves_bytes() {
        let id = uuid::Uuid::parse_str("12345678-1234-5678-9abc-123456789abc").unwrap();
        let v = Value::from_reader(std::io::Cursor::new(nsuuid(id).unwrap())).unwrap();
        let objects = v.as_dictionary().unwrap()["$objects"].as_array().unwrap();
        assert_eq!(
            objects[1].as_dictionary().unwrap()["NS.uuidbytes"]
                .as_data()
                .unwrap(),
            id.as_bytes()
        );
        assert_eq!(
            objects[2].as_dictionary().unwrap()["$classname"].as_string(),
            Some("NSUUID")
        );
    }
    #[test]
    fn capabilities_use_object_references() {
        let mut caps = Dictionary::new();
        caps.insert("test".into(), true.into());
        let v = Value::from_reader(std::io::Cursor::new(capabilities(caps).unwrap())).unwrap();
        let objects = v.as_dictionary().unwrap()["$objects"].as_array().unwrap();
        assert_eq!(
            objects[2].as_dictionary().unwrap()["$classname"].as_string(),
            Some("XCTCapabilities")
        );
        assert_eq!(
            objects[3].as_dictionary().unwrap()["NS.keys"]
                .as_array()
                .unwrap(),
            &[uid(5)]
        );
        assert_eq!(objects[5].as_string(), Some("test"));
        assert_eq!(objects[6].as_boolean(), Some(true));
    }
}
