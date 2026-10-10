//! Stable semantic anchors for observing scroll progress.

use crate::wda::ElementRow;
use std::collections::HashMap;

/// Whether a unique, labelled Cell/StaticText/Other/Image moved with the scroll.
///
/// `dx`/`dy` use the `scroll` content-reveal convention: positive means reveal
/// right/below, so existing content must move left/up (negative coordinates).
/// `wda_swipe` uses start = centre + travel/2, end = centre - travel/2;
/// page and element scrolls use the same sign. For direct `swipe`/`drag`, pass
/// start minus end, since content follows the finger from start to end.
/// The larger absolute delta selects the axis (ties select vertical).
/// Returns `None` for no common unique anchor or an invalid/zero direction.
/// Also returns `None` when newly appeared
/// labelled candidate rows (by kind, label, identifier) outnumber common unique
/// anchors, or exceed both two rows and 20% of the common unique anchor count,
/// as in full-page or virtualized updates. Smaller label changes can still yield
/// `Some(false)` when common anchors exist.
/// Value changes are irrelevant; size changes and cross-axis motion do not
/// prove progress. `Some(false)` does not mean the list has been read completely.
pub fn scroll_anchor_moved(
    baseline: &[ElementRow],
    current: &[ElementRow],
    dx: f64,
    dy: f64,
) -> Option<bool> {
    scroll_anchor_progress(baseline, current, dx, dy).map(|(moved, _, _)| moved)
}

/// Scroll motion evidence, still anchor count, and common unique anchor count.
/// A still anchor has its origin (x, y) within 2.0pt of baseline, ignoring size.
pub fn scroll_anchor_progress(
    baseline: &[ElementRow],
    current: &[ElementRow],
    dx: f64,
    dy: f64,
) -> Option<(bool, usize, usize)> {
    if !dx.is_finite() || !dy.is_finite() || (dx == 0.0 && dy == 0.0) {
        return None;
    }
    type Key<'a> = (&'a str, &'a str, Option<&'a str>);
    fn anchors(rows: &[ElementRow]) -> HashMap<Key<'_>, Option<&ElementRow>> {
        let mut anchors = HashMap::new();
        for row in rows {
            if !row.label.is_empty()
                && matches!(row.kind.as_str(), "Cell" | "StaticText" | "Other" | "Image")
            {
                anchors
                    .entry((
                        row.kind.as_str(),
                        row.label.as_str(),
                        row.identifier.as_deref(),
                    ))
                    .and_modify(|entry| *entry = None)
                    .or_insert(Some(row));
            }
        }
        anchors
    }
    let axis = if dx.abs() > dy.abs() { 0 } else { 1 };
    let direction = -if axis == 0 { dx } else { dy }.signum();
    let before = anchors(baseline);
    let after = anchors(current);
    let mut common_anchor_count = 0;
    let mut still_anchor_count = 0;
    let mut moved = false;
    for (key, row) in &before {
        let (Some(row), Some(Some(next))) = (row, after.get(key)) else {
            continue;
        };
        common_anchor_count += 1;
        if next
            .rect
            .iter()
            .take(2)
            .zip(row.rect.iter())
            .all(|(next, before)| (next - before).abs() <= 2.0)
        {
            still_anchor_count += 1;
        }
        if (next.rect[axis] - row.rect[axis]) * direction > 2.0
            && (next.rect[1 - axis] - row.rect[1 - axis]).abs() <= 3.0
            && (next.rect[2] - row.rect[2]).abs() <= 2.0
            && (next.rect[3] - row.rect[3]).abs() <= 2.0
        {
            moved = true;
        }
    }
    let appeared = current
        .iter()
        .filter(|row| {
            !row.label.is_empty()
                && matches!(row.kind.as_str(), "Cell" | "StaticText" | "Other" | "Image")
                && !before.contains_key(&(
                    row.kind.as_str(),
                    row.label.as_str(),
                    row.identifier.as_deref(),
                ))
        })
        .count();
    if appeared > common_anchor_count || (appeared > 2 && appeared * 5 > common_anchor_count) {
        return None;
    }
    (common_anchor_count > 0).then_some((moved, still_anchor_count, common_anchor_count))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> ElementRow {
        ElementRow {
            kind: "Cell".into(),
            label: "Stable item".into(),
            rect: [20.0, 100.0, 80.0, 40.0],
            ..Default::default()
        }
    }

    #[test]
    fn scroll_progress_still_ignores_size_but_requires_both_origin_axes() {
        let mut next = row();
        next.rect[2] += 20.0;
        assert_eq!(
            scroll_anchor_progress(&[row()], &[next.clone()], 0.0, 100.0),
            Some((false, 1, 1))
        );
        next.rect[0] += 2.1;
        assert_eq!(
            scroll_anchor_progress(&[row()], &[next], 0.0, 100.0),
            Some((false, 0, 1))
        );
    }

    #[test]
    fn scroll_progress_fling_guard_even_with_moving_anchor() {
        let mut next = row();
        next.rect[1] -= 20.0;
        let mut current = vec![next];
        for i in 0..3 {
            let mut new = row();
            new.label = format!("new {i}");
            current.push(new);
        }
        assert_eq!(scroll_anchor_progress(&[row()], &current, 0.0, 100.0), None);
    }

    #[test]
    fn scroll_progress_full_page_replacement_is_unknown() {
        let mut title = row();
        title.kind = "StaticText".into();
        title.label = "Fixed title".into();
        let mut baseline = vec![title.clone()];
        let mut current = vec![title];
        for index in 0..6 {
            let mut old = row();
            old.label = format!("Old item {index}");
            old.rect[1] += index as f64 * 40.0;
            let mut new = old.clone();
            new.label = format!("New item {index}");
            baseline.push(old);
            current.push(new);
        }
        assert_eq!(scroll_anchor_moved(&baseline, &current, 0.0, 100.0), None);
    }

    #[test]
    fn scroll_progress_small_timestamp_changes_are_not_progress() {
        for count in 1..=2 {
            let mut baseline = vec![row()];
            let mut current = baseline.clone();
            for index in 0..count {
                let mut old = row();
                old.kind = "StaticText".into();
                old.label = format!("Timestamp {index}: 10:00");
                let mut new = old.clone();
                new.label = format!("Timestamp {index}: 10:01");
                baseline.push(old);
                current.push(new);
            }
            assert_eq!(
                scroll_anchor_moved(&baseline, &current, 0.0, 100.0),
                if count == 1 { Some(false) } else { None }
            );
        }
    }

    #[test]
    fn scroll_progress_two_cell_replacement_is_unknown() {
        let mut title = row();
        title.kind = "StaticText".into();
        title.label = "Fixed title".into();
        let mut baseline = vec![title.clone()];
        let mut current = vec![title];
        for (index, (old_label, new_label)) in [("A", "C"), ("B", "D")].into_iter().enumerate() {
            let mut old = row();
            old.label = old_label.into();
            old.rect = [0.0, 100.0 + index as f64 * 300.0, 393.0, 300.0];
            let mut new = old.clone();
            new.label = new_label.into();
            baseline.push(old);
            current.push(new);
        }
        assert_eq!(scroll_anchor_progress(&baseline, &current, 0.0, 100.0), None);
    }

    #[test]
    fn scroll_progress_replacement_requires_more_than_twenty_percent() {
        for common_count in [14, 15, 16] {
            let baseline: Vec<_> = (0..common_count)
                .map(|index| {
                    let mut item = row();
                    item.label = format!("Unchanged {index}");
                    item
                })
                .collect();
            let mut current = baseline.clone();
            for index in 0..3 {
                let mut item = row();
                item.label = format!("Appeared {index}");
                current.push(item);
            }
            assert_eq!(
                scroll_anchor_moved(&baseline, &current, 0.0, 100.0),
                if common_count < 15 { None } else { Some(false) }
            );
        }
    }

    #[test]
    fn scroll_progress_down_moves_content_up() {
        let mut next = row();
        next.rect[1] -= 10.0;
        assert_eq!(
            scroll_anchor_moved(&[row()], &[next.clone()], 0.0, 100.0),
            Some(true)
        );
        assert_eq!(
            scroll_anchor_moved(&[row()], &[next], 0.0, -100.0),
            Some(false)
        );
    }

    #[test]
    fn scroll_progress_value_only_is_not_motion() {
        let mut next = row();
        next.value = Some("new price".into());
        assert_eq!(
            scroll_anchor_moved(&[row()], &[next], 0.0, 100.0),
            Some(false)
        );
    }

    #[test]
    fn scroll_progress_duplicate_in_either_tree_is_ignored() {
        let single = vec![row()];
        let duplicate = vec![row(), row()];
        assert_eq!(scroll_anchor_moved(&single, &duplicate, 0.0, 1.0), None);
        assert_eq!(scroll_anchor_moved(&duplicate, &single, 0.0, 1.0), None);
        let mut unique = row();
        unique.identifier = Some("unique".into());
        let mut before = duplicate.clone();
        before.push(unique.clone());
        let mut after = duplicate;
        after[0].rect[1] -= 20.0;
        after.push(unique);
        assert_eq!(scroll_anchor_moved(&before, &after, 0.0, 1.0), Some(false));
    }

    #[test]
    fn scroll_progress_no_common_anchor() {
        let mut next = row();
        next.label = "different".into();
        assert_eq!(scroll_anchor_moved(&[row()], &[next], 0.0, 1.0), None);
        for kind in ["Button", "Application"] {
            let mut excluded = row();
            excluded.kind = kind.into();
            assert_eq!(
                scroll_anchor_moved(&[excluded.clone()], &[excluded], 0.0, 1.0),
                None
            );
        }
        let mut empty = row();
        empty.label.clear();
        assert_eq!(
            scroll_anchor_moved(&[empty.clone()], &[empty], 0.0, 1.0),
            None
        );
    }

    #[test]
    fn scroll_progress_horizontal_and_dominant_axis() {
        let mut next = row();
        next.rect[0] += 10.0;
        assert_eq!(
            scroll_anchor_moved(&[row()], &[next.clone()], -100.0, 1.0),
            Some(true)
        );
        assert_eq!(
            scroll_anchor_moved(&[row()], &[next], -1.0, 100.0),
            Some(false)
        );
    }

    #[test]
    fn scroll_progress_size_change_is_not_motion() {
        for axis in [2, 3] {
            let mut next = row();
            next.rect[1] -= 10.0;
            next.rect[axis] += 2.1;
            assert_eq!(
                scroll_anchor_moved(&[row()], &[next], 0.0, 1.0),
                Some(false)
            );
        }
    }

    #[test]
    fn scroll_progress_thresholds_and_invalid_direction() {
        let mut next = row();
        next.rect[1] -= 2.0;
        assert_eq!(
            scroll_anchor_moved(&[row()], &[next.clone()], 0.0, 1.0),
            Some(false)
        );
        next.rect[1] -= 0.1;
        next.rect[0] += 3.0;
        next.rect[2] += 2.0;
        assert_eq!(
            scroll_anchor_moved(&[row()], &[next.clone()], 0.0, 1.0),
            Some(true)
        );
        next.rect[0] += 0.1;
        assert_eq!(
            scroll_anchor_moved(&[row()], &[next], 0.0, 1.0),
            Some(false)
        );
        for (dx, dy) in [(0.0, 0.0), (f64::NAN, 1.0), (0.0, f64::INFINITY)] {
            assert_eq!(scroll_anchor_moved(&[row()], &[row()], dx, dy), None);
        }
    }
}
