//! Completeness contract shared by the Native-to-daemon and daemon-to-MCP reads.
use serde_json::Value;

pub fn valid_contract(report: &Value, limit: u32) -> bool {
    if !(1..=65_536).contains(&limit) {
        return false;
    }
    let Some(value) = report.get("value") else {
        return false;
    };
    if !value.is_null() && !value.is_string() {
        return false;
    }
    let returned = value
        .as_str()
        .map_or(0, |value| value.encode_utf16().count()) as u64;
    let Some(total) = report.get("utf16_length").and_then(Value::as_u64) else {
        return false;
    };
    returned <= u64::from(limit)
        && total >= returned
        && report["source"] == "ax_value_attribute"
        && report["limit_utf16"] == limit
        && report["returned_utf16_length"] == returned
        && report["complete"] == (returned == total)
        && report["truncated"] == (returned < total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn utf16_lengths_and_completeness_are_independent_of_unicode_scalar_counts() {
        let full = json!({"value":"😀a","source":"ax_value_attribute","utf16_length":3,
            "returned_utf16_length":3,"limit_utf16":8192,"complete":true,"truncated":false});
        assert!(valid_contract(&full, 8192));
        for (key, value) in [
            ("returned_utf16_length", json!(2)),
            ("utf16_length", json!(2)),
            ("complete", json!(false)),
            ("truncated", json!(true)),
            ("limit_utf16", json!(100)),
            ("source", json!("snapshot")),
        ] {
            let mut bad = full.clone();
            bad[key] = value;
            assert!(!valid_contract(&bad, 8192), "{key}: {bad}");
        }
        let clipped = json!({"value":"","source":"ax_value_attribute","utf16_length":2,
            "returned_utf16_length":0,"limit_utf16":1,"complete":false,"truncated":true});
        assert!(valid_contract(&clipped, 1));
        let empty = json!({"value":null,"source":"ax_value_attribute","utf16_length":0,
            "returned_utf16_length":0,"limit_utf16":1,"complete":true,"truncated":false});
        assert!(valid_contract(&empty, 1));
        assert!(!valid_contract(&json!({"value":"PREVIEW"}), 8192));
        assert!(!valid_contract(&full, 0));
    }
}
