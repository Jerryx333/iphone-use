//! Read-only SpringBoard icon-state decoding. Never emits a restore payload.
use serde_json::{json, Value};

fn dimension(metrics: &Value, key: &str) -> Result<usize, String> {
    metrics[key]
        .as_u64()
        .filter(|n| (1..=40).contains(n))
        .map(|n| n as usize)
        .ok_or_else(|| format!("missing or invalid {key}"))
}

pub fn decode(state: &Value, metrics: &Value) -> Result<Value, String> {
    let pages = state
        .as_array()
        .filter(|p| (2..=128).contains(&p.len()))
        .ok_or("icon state must contain a Dock and home pages")?;
    let rows = dimension(metrics, "homeScreenIconRows")?;
    let cols = dimension(metrics, "homeScreenIconColumns")?;
    let folder_rows = dimension(metrics, "homeScreenIconFolderRows")?;
    let folder_cols = dimension(metrics, "homeScreenIconFolderColumns")?;
    let dock_cols = dimension(metrics, "homeScreenIconDockMaxCount")?;
    let mut budget = 10_000;
    let mut decode_page = |page: &Value, r, c, path: &str| {
        page_layout(page, r, c, folder_rows, folder_cols, path, 0, &mut budget)
    };
    let dock = decode_page(&pages[0], 1, dock_cols, "dock")?;
    let mut home = Vec::new();
    for (index, page) in pages[1..].iter().enumerate() {
        let mut decoded = decode_page(page, rows, cols, &format!("pages/{index}"))?;
        decoded["page"] = json!(index);
        home.push(decoded);
    }
    Ok(json!({
        "source": "springboardservices", "read_only": true,
        "index_base": 0, "page_index_space":"persisted_icon_state", "page_visibility":"not_reported", "position_source": "icon_state_order_and_grid_metrics",
        "position_note": "Page indexes include persisted pages hidden by Focus or Home Screen settings and are not the visible page-control indexes. Grid positions are derived from icon order and widget spans; no pixel coordinates or restoration guarantee.",
        "grid": {"rows":rows,"columns":cols},
        "folder_grid": {"rows":folder_rows,"columns":folder_cols},
        "dock": dock, "pages": home,
    }))
}

#[allow(clippy::too_many_arguments)]
fn page_layout(
    page: &Value,
    rows: usize,
    cols: usize,
    folder_rows: usize,
    folder_cols: usize,
    path: &str,
    depth: usize,
    budget: &mut usize,
) -> Result<Value, String> {
    if depth > 4 {
        return Err("icon-state nesting exceeds four folders".into());
    }
    let items = page
        .as_array()
        .filter(|p| p.len() <= 512)
        .ok_or("invalid icon page")?;
    let mut occupied = vec![false; rows * cols];
    let mut cursor = 0;
    let mut positioned = true;
    let mut entries = Vec::new();
    for (index, item) in items.iter().enumerate() {
        if *budget == 0 {
            return Err("icon state exceeds 10000 entries".into());
        }
        *budget -= 1;
        let item_path = format!("{path}/{index}");
        let folder = item
            .get("iconLists")
            .and_then(Value::as_array)
            .filter(|lists| !lists.is_empty() || item.get("listType").is_some());
        let widget = item.get("gridSize").is_some() || item["iconType"] == "custom";
        let kind = if folder.is_some() {
            "folder"
        } else if widget {
            "widget"
        } else if item.get("webClipURL").is_some() {
            "web_clip"
        } else if item
            .get("bundleIdentifier")
            .and_then(Value::as_str)
            .is_some()
            || item
                .get("displayIdentifier")
                .and_then(Value::as_str)
                .is_some()
        {
            "app"
        } else {
            "unknown"
        };
        let span = if widget {
            match item["gridSize"].as_str() {
                Some("small") => Some((2, 2)),
                Some("medium") => Some((2, 4)),
                Some("large") => Some((4, 4)),
                _ => None,
            }
        } else if kind == "unknown" {
            None
        } else {
            Some((1, 1))
        };
        let place = if positioned {
            span.and_then(|(height, width)| {
                (cursor..rows * cols)
                    .find(|&cell| {
                        let (r, c) = (cell / cols, cell % cols);
                        r + height <= rows
                            && c + width <= cols
                            && (r..r + height)
                                .all(|y| (c..c + width).all(|x| !occupied[y * cols + x]))
                    })
                    .map(|cell| {
                        let (r, c) = (cell / cols, cell % cols);
                        for y in r..r + height {
                            for x in c..c + width {
                                occupied[y * cols + x] = true;
                            }
                        }
                        cursor = cell + 1;
                        (r, c)
                    })
            })
        } else {
            None
        };
        if place.is_none() {
            positioned = false;
        }
        let mut entry = json!({"index":index,"path":item_path,"kind":kind,
            "row":place.map(|p|p.0),"col":place.map(|p|p.1),
            "size":span.map(|s|json!({"rows":s.0,"columns":s.1})),
            "label":item.get("displayName").and_then(Value::as_str),
            "bundle_id":item.get("bundleIdentifier").and_then(Value::as_str),
            "identifier":item.get("displayIdentifier").and_then(Value::as_str),
        });
        if widget {
            entry["grid_size"] = item.get("gridSize").cloned().unwrap_or(Value::Null);
            // A widget stack retains its members, without copying unrelated
            // state (dates, web-clip URLs, suggestion settings, etc.).
            if let Some(members) = item.get("elements").and_then(Value::as_array) {
                if members.len() > 128 {
                    return Err("widget stack exceeds 128 members".into());
                }
                entry["members"] = json!(members
                    .iter()
                    .map(|m| json!({
                        "bundle_id":m.get("bundleIdentifier").and_then(Value::as_str),
                        "widget_identifier":m.get("widgetIdentifier").and_then(Value::as_str),
                        "element_type":m.get("elementType").and_then(Value::as_str),
                    }))
                    .collect::<Vec<_>>());
            }
            entry["widget_identifier"] =
                item.get("widgetIdentifier").cloned().unwrap_or(Value::Null);
            entry["element_type"] = item.get("elementType").cloned().unwrap_or(Value::Null);
        }
        if let Some(lists) = folder {
            if lists.len() > 128 {
                return Err("folder exceeds 128 pages".into());
            }
            let mut folder_pages = Vec::new();
            for (page_index, page) in lists.iter().enumerate() {
                let mut decoded = page_layout(
                    page,
                    folder_rows,
                    folder_cols,
                    folder_rows,
                    folder_cols,
                    &format!("{item_path}/pages/{page_index}"),
                    depth + 1,
                    budget,
                )?;
                decoded["page"] = json!(page_index);
                folder_pages.push(decoded);
            }
            entry["pages"] = json!(folder_pages);
        }
        entries.push(entry);
    }
    Ok(json!({"entries":entries,"positions_complete":positioned}))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn metrics() -> Value {
        json!({"homeScreenIconRows":6,"homeScreenIconColumns":4,
        "homeScreenIconFolderRows":3,"homeScreenIconFolderColumns":3,"homeScreenIconDockMaxCount":4})
    }
    fn app(n: usize) -> Value {
        json!({"displayName":format!("App {n}"),"bundleIdentifier":format!("example.app{n}")})
    }
    #[test]
    fn dock_widgets_and_all_folder_pages_preserve_source_order() {
        let state = json!([[app(0)],[{"iconType":"custom","gridSize":"small"},
            {"iconType":"custom","gridSize":"small"},app(1),
            {"displayName":"Folder","listType":"folder","iconLists":[[app(2)],[app(3),app(4)]]}]]);
        let out = decode(&state, &metrics()).unwrap();
        assert_eq!(out["dock"]["entries"][0]["row"], 0);
        let entries = &out["pages"][0]["entries"];
        assert_eq!(entries[1]["row"], 0);
        assert_eq!(entries[1]["col"], 2);
        assert_eq!(entries[2]["row"], 2);
        assert_eq!(entries[2]["col"], 0);
        assert_eq!(entries[3]["pages"][1]["entries"][1]["col"], 1);
        assert_eq!(
            entries[3]["pages"][1]["entries"][0]["bundle_id"],
            "example.app3"
        );
        assert_eq!(out["pages"][0]["positions_complete"], true);
    }
    #[test]
    fn unknown_span_keeps_items_but_does_not_invent_positions_after_it() {
        for unknown in [
            json!({"iconType":"custom","gridSize":"future"}),
            json!(false),
        ] {
            let out = decode(&json!([[], [app(1), unknown, app(2)]]), &metrics()).unwrap();
            assert_eq!(out["pages"][0]["positions_complete"], false);
            assert!(out["pages"][0]["entries"][2]["row"].is_null());
            assert_eq!(out["pages"][0]["entries"].as_array().unwrap().len(), 3);
        }
    }
    #[test]
    fn malformed_state_and_missing_metrics_are_not_an_empty_layout() {
        for state in [
            json!({"error":"denied"}),
            json!([]),
            json!([[]]),
            json!([[], {}]),
        ] {
            assert!(decode(&state, &metrics()).is_err());
        }
        assert!(decode(&json!([[], [app(1)]]), &json!({})).is_err());
    }
    #[test]
    fn widget_stack_members_and_web_clips_are_distinct() {
        let out=decode(&json!([[],[{"iconType":"custom","gridSize":"medium","elements":[
            {"bundleIdentifier":"example.widget","widgetIdentifier":"widget.type"}]},
            {"displayName":"Site","displayIdentifier":"clip-id","webClipURL":"https://private.example/secret"}]]),&metrics()).unwrap();
        assert_eq!(
            out["pages"][0]["entries"][0]["members"][0]["bundle_id"],
            "example.widget"
        );
        assert_eq!(out["pages"][0]["entries"][1]["kind"], "web_clip");
        assert!(!out.to_string().contains("private.example"));
    }
}
