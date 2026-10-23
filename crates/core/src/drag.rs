//! A bounded, single-finger drag with stops along its path.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const MAX_POINTS: usize = 16;
pub const MAX_DURATION_MS: u64 = 10_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Point {
    pub x: f64,
    pub y: f64,
    #[serde(default)]
    pub dwell_ms: u64,
}

#[derive(Debug)]
pub struct Path {
    pub points: Vec<Point>,
    pub hold_ms: u64,
    /// Travel time per segment, excluding each point's dwell.
    pub duration_ms: u64,
}

impl Path {
    pub fn parse(action: &Value) -> Result<Self, &'static str> {
        if ["x1", "y1", "x2", "y2"]
            .iter()
            .any(|key| action.get(key).is_some())
        {
            return Err("drag needs either points or x1/y1/x2/y2, not both");
        }
        let points: Vec<Point> = serde_json::from_value(
            action.get("points").cloned().unwrap_or(Value::Null),
        )
        .map_err(|_| "drag points need x/y coordinates and optional integer dwell_ms only")?;
        let ms = |key: &str, default: u64| match action.get(key) {
            None => Ok(default),
            Some(value) => value
                .as_u64()
                .ok_or("drag timing must be a non-negative integer"),
        };
        let path = Self {
            points,
            hold_ms: ms("hold_ms", 500)?,
            duration_ms: ms("duration_ms", 300)?,
        };
        path.validate()?;
        Ok(path)
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if !(2..=MAX_POINTS).contains(&self.points.len()) {
            return Err("drag points must contain 2 to 16 points");
        }
        if self.points.iter().any(|p| {
            ![p.x, p.y]
                .iter()
                .all(|v| v.is_finite() && (0.0..=1.0).contains(v))
        }) {
            return Err("drag point coordinates must be finite numbers from 0 to 1");
        }
        if !self
            .points
            .windows(2)
            .any(|pair| pair[0].x != pair[1].x || pair[0].y != pair[1].y)
        {
            return Err("drag path must move between at least two distinct points");
        }
        if !(80..=2_000).contains(&self.duration_ms) || self.hold_ms > 2_000 {
            return Err("drag path duration_ms must be 80 to 2000 and hold_ms at most 2000");
        }
        let total = self.points.iter().try_fold(
            self.hold_ms + self.duration_ms * (self.points.len() - 1) as u64,
            |sum, p| sum.checked_add(p.dwell_ms),
        );
        if total.is_none_or(|total| total > MAX_DURATION_MS) {
            return Err("drag path hold, travel and dwell must total at most 10000 ms");
        }
        Ok(())
    }

    /// One down/up pair; all intermediate pauses keep the same finger down.
    pub fn actions(&self, width: f64, height: f64) -> Result<Value, &'static str> {
        self.validate()?;
        if ![width, height].iter().all(|v| v.is_finite() && *v >= 1.0) {
            return Err("drag needs valid screen dimensions");
        }
        let position = |p: &Point| {
            (
                (p.x * width).min(width - 1.0),
                (p.y * height).min(height - 1.0),
            )
        };
        let (x, y) = position(&self.points[0]);
        let mut steps = vec![
            json!({"type":"pointerMove","duration":0,"x":x,"y":y}),
            json!({"type":"pointerDown","button":0}),
        ];
        let initial_hold = self.hold_ms + self.points[0].dwell_ms;
        if initial_hold > 0 {
            steps.push(json!({"type":"pause","duration":initial_hold}));
        }
        for point in &self.points[1..] {
            let (x, y) = position(point);
            steps.push(json!({"type":"pointerMove","duration":self.duration_ms,"x":x,"y":y}));
            if point.dwell_ms > 0 {
                steps.push(json!({"type":"pause","duration":point.dwell_ms}));
            }
        }
        steps.push(json!({"type":"pointerUp","button":0}));
        Ok(json!(steps))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_keeps_one_finger_down_through_its_edge_dwell() {
        let path = Path::parse(&json!({"points":[{"x":0.3,"y":0.4},{"x":0.13,"y":0.5,"dwell_ms":900},{"x":1,"y":0.4}],"hold_ms":400})).unwrap();
        let steps = path.actions(390.0, 844.0).unwrap();
        let steps = steps.as_array().unwrap();
        assert_eq!(
            steps.iter().filter(|s| s["type"] == "pointerDown").count(),
            1
        );
        assert_eq!(steps.iter().filter(|s| s["type"] == "pointerUp").count(), 1);
        assert_eq!(steps[2], json!({"type":"pause","duration":400}));
        assert_eq!(steps[4], json!({"type":"pause","duration":900}));
        assert_eq!(steps[5]["x"], 389.0);
        assert_eq!(steps.last().unwrap()["type"], "pointerUp");
    }

    #[test]
    fn invalid_or_ambiguous_paths_are_rejected() {
        let valid = json!({"points":[{"x":0.1,"y":0.2},{"x":0.9,"y":0.2}]});
        for change in [
            json!({"points":[]}),
            json!({"points":null}),
            json!({"x1":0.1}),
            json!({"hold_ms":-1}),
            json!({"hold_ms":null}),
            json!({"duration_ms":0}),
            json!({"duration_ms":79}),
            json!({"duration_ms":2001}),
            json!({"hold_ms":2001}),
            json!({"duration_ms":10001}),
            json!({"points":[{"x":0.1,"y":0.2},{"x":1.1,"y":0.2}]}),
            json!({"points":[{"x":0.1,"y":0.2},{"x":0.1,"y":0.2}]}),
            json!({"points":[{"x":0.1,"y":0.2,"dwell_ms":u64::MAX},{"x":0.9,"y":0.2}]}),
            json!({"points":[{"x":0.1,"y":0.2},{"x":0.9,"y":0.2,"unknown":1}]}),
        ] {
            let mut action = valid.clone();
            for (key, value) in change.as_object().unwrap() {
                action[key] = value.clone();
            }
            assert!(Path::parse(&action).is_err(), "{action}");
        }
        let points = vec![json!({"x":0.1,"y":0.2}); MAX_POINTS + 1];
        assert!(Path::parse(&json!({"points":points})).is_err());
        let path = Path::parse(&valid).unwrap();
        assert!(path.actions(f64::NAN, 844.0).is_err());
        assert!(path.actions(0.5, 844.0).is_err());
    }
}
