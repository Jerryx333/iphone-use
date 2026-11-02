//! Learned skills: how a task that once worked gets done again.
//!
//! A skill is a short markdown how-to the driving agent writes after it
//! finished a task on the phone: entry, steps, pitfalls, how to tell it is
//! done, when to hand over. Unlike a flow it is read by a model, not replayed
//! tap by tap, so it survives small UI changes. The life of one:
//!
//! 1. **Draft** — the release reply (`learn` block) asks the agent to write
//!    one when the session drove an app for several steps; `draft` stores it
//!    under `drafts/`. A draft is never shown to later runs.
//! 2. **Trial** — the user accepts it (`accept`); the `registry` block of its
//!    apps now lists it. The first verified run makes it **active**, a failed
//!    one **suspends** it.
//! 3. **Active** — every run that followed it reports a `result`; two of the
//!    last three reviewed runs not verified suspend it. A suspended skill is
//!    hidden until `resume`.
//!
//! Re-drafting a skill of the same kind (`updates:` or the same `name`)
//! replaces it on accept and keeps the old text as `<name>.prev.md` for
//! `restore`. Files live beside `app-notes/`, outside the flow store that
//! `flow update` rewrites.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Longest skill text accepted (characters, front matter included).
pub const MAX_CHARS: usize = 4000;
/// Outcomes kept per skill.
pub const KEEP: usize = 5;
const WINDOW: usize = 3;
const BAD_IN_WINDOW: usize = 2;
/// Most skills listed in one `registry` block.
const IN_BLOCK: usize = 8;
const STATE_FILE: &str = ".state.json";
const DRAFTS: &str = "drafts";

/// The five sections, each accepted by its Chinese or English heading.
const SECTIONS: [(&str, &[&str]); 5] = [
    ("entry", &["入口", "entry"]),
    ("steps", &["步骤", "steps"]),
    ("pitfalls", &["容易出错的地方", "pitfalls"]),
    ("done", &["怎样算办成", "done when"]),
    ("hand over", &["什么时候停下交给用户", "hand over"]),
];

pub fn skills_dir() -> PathBuf {
    crate::flows::app_notes_dir()
        .parent()
        .map(|parent| parent.join("skills"))
        .unwrap_or_else(|| crate::instance::current().state_dir.join("skills"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Trial,
    Active,
    Suspended,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Status::Trial => "trial",
            Status::Active => "active",
            Status::Suspended => "suspended",
        }
    }
}

/// How one run that followed a skill went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Done, and the end screen showed it.
    Verified,
    /// Did not get done, or only partly.
    Failed,
    /// Stopped for reasons unrelated to the skill (phone locked, user took
    /// over, model error). Never counts toward suspension.
    Unreviewed,
}

impl Outcome {
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim() {
            "verified" | "done" | "ok" => Some(Outcome::Verified),
            "failed" | "partial" => Some(Outcome::Failed),
            "unreviewed" | "unknown" => Some(Outcome::Unreviewed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub status: Status,
    #[serde(default)]
    pub outcomes: Vec<Outcome>,
    #[serde(default)]
    pub accepted_at: u64,
}

impl Record {
    /// Add one outcome; may move trial → active / suspended, or suspend an
    /// active skill that keeps failing.
    pub fn record(&mut self, outcome: Outcome) {
        self.outcomes.push(outcome);
        let excess = self.outcomes.len().saturating_sub(KEEP);
        self.outcomes.drain(..excess);
        self.status = match (self.status, outcome) {
            (Status::Trial, Outcome::Verified) => Status::Active,
            (Status::Trial, Outcome::Failed) => Status::Suspended,
            (Status::Active, _) if should_suspend(&self.outcomes) => Status::Suspended,
            (status, _) => status,
        };
    }
}

/// Two of the last three reviewed outcomes not verified.
pub fn should_suspend(outcomes: &[Outcome]) -> bool {
    outcomes
        .iter()
        .filter(|o| **o != Outcome::Unreviewed)
        .rev()
        .take(WINDOW)
        .filter(|o| **o != Outcome::Verified)
        .count()
        >= BAD_IN_WINDOW
}

/// What the front matter says.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Meta {
    pub name: String,
    pub display: Option<String>,
    pub description: String,
    pub apps: Vec<String>,
    pub aliases: Vec<String>,
    pub updates: Option<String>,
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn list_value(raw: &str) -> Vec<String> {
    raw.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(|s| s.trim().trim_matches(|c| c == '"' || c == '\'').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Split `---` front matter from the body; `None` without one.
fn split(text: &str) -> Option<(&str, &str)> {
    let rest = text.trim_start().strip_prefix("---")?;
    let end = rest.find("\n---")?;
    let body = &rest[end + 4..];
    Some((&rest[..end], body.strip_prefix('\n').unwrap_or(body)))
}

pub fn parse_meta(text: &str) -> Option<Meta> {
    let (head, _) = split(text)?;
    let mut meta = Meta::default();
    for line in head.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
        match key.trim() {
            "name" => meta.name = value.to_string(),
            "display" if !value.is_empty() => meta.display = Some(value.to_string()),
            "description" => meta.description = value.to_string(),
            "apps" => meta.apps = list_value(value),
            "aliases" => meta.aliases = list_value(value),
            "updates" if !value.is_empty() => meta.updates = Some(value.to_string()),
            _ => {}
        }
    }
    Some(meta)
}

/// Long digit runs (phone, card, id numbers) and e-mail addresses: things a
/// skill about "this kind of task" never needs.
fn looks_personal(text: &str) -> Option<&'static str> {
    let mut run = 0;
    for c in text.chars() {
        if c.is_ascii_digit() {
            run += 1;
            if run >= 11 {
                return Some("a long number (phone, card or id)");
            }
        } else if !matches!(c, ' ' | '-') {
            run = 0;
        }
    }
    let email = text.split_whitespace().any(|word| {
        word.split_once('@').is_some_and(|(user, host)| {
            !user.is_empty() && host.contains('.') && !host.starts_with('.')
        })
    });
    email.then_some("an e-mail address")
}

/// Check a drafted skill; `Err` says what to fix.
pub fn validate(text: &str) -> Result<Meta, String> {
    if text.chars().count() > MAX_CHARS {
        return Err(format!(
            "the skill is {} characters; keep it under {MAX_CHARS}: the general way to do this kind of task, not a log of this run",
            text.chars().count()
        ));
    }
    let meta = parse_meta(text)
        .ok_or("start with --- front matter (name, display, description, apps, aliases) and close it with ---")?;
    if !valid_name(&meta.name) {
        return Err(
            "name: lowercase letters, digits and hyphens only, e.g. send-wechat-message".into(),
        );
    }
    if meta.description.trim().is_empty() {
        return Err("description: one line saying when to use this skill".into());
    }
    if meta.apps.is_empty()
        || !meta
            .apps
            .iter()
            .all(|b| crate::flows::valid_app_notes_bundle(b))
    {
        return Err(
            "apps: the bundle ids it uses, e.g. [com.tencent.xin] (GET /agent/apps finds them)"
                .into(),
        );
    }
    if let Some(updates) = &meta.updates {
        if !valid_name(updates) {
            return Err("updates: the name of the skill this one rewrites".into());
        }
    }
    let (_, body) = split(text).unwrap_or(("", text));
    let headings: Vec<String> = body
        .lines()
        .filter_map(|l| l.trim().strip_prefix("## "))
        .map(|h| h.trim().to_lowercase())
        .collect();
    let missing: Vec<&str> = SECTIONS
        .iter()
        .filter(|(_, names)| {
            !headings
                .iter()
                .any(|h| names.iter().any(|n| h.starts_with(&n.to_lowercase())))
        })
        .map(|(label, _)| *label)
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "needs five `## ` sections: 入口 / 步骤 / 容易出错的地方 / 怎样算办成 / 什么时候停下交给用户 \
             (or Entry / Steps / Pitfalls / Done when / Hand over); missing: {}",
            missing.join(", ")
        ));
    }
    if let Some(what) = looks_personal(text) {
        return Err(format!(
            "it contains {what}; write `<参数>` or \"the contact the user names\" instead of this run's values"
        ));
    }
    Ok(meta)
}

/// The skill store in one directory.
pub struct Store {
    dir: PathBuf,
}

/// One skill as listed.
#[derive(Debug, Clone)]
pub struct Entry {
    pub meta: Meta,
    pub status: Option<Status>,
    pub outcomes: Vec<Outcome>,
    pub draft: bool,
    pub has_previous: bool,
}

impl Entry {
    pub fn to_json(&self) -> Value {
        let mut row = json!({
            "name": self.meta.name,
            "description": self.meta.description,
            "apps": self.meta.apps,
            "status": if self.draft { "draft" } else { self.status.map_or("unknown", Status::as_str) },
        });
        if let Some(display) = &self.meta.display {
            row["display"] = json!(display);
        }
        if !self.meta.aliases.is_empty() {
            row["aliases"] = json!(self.meta.aliases);
        }
        if !self.outcomes.is_empty() {
            row["recent"] = json!(self.outcomes);
        }
        if let Some(updates) = &self.meta.updates {
            row["updates"] = json!(updates);
        }
        if self.has_previous {
            row["has_previous"] = json!(true);
        }
        row
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

fn remove_if_present(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

impl Store {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn open() -> Self {
        Self::new(skills_dir())
    }

    fn live_path(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.md"))
    }
    fn prev_path(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.prev.md"))
    }
    fn draft_path(&self, name: &str) -> PathBuf {
        self.dir.join(DRAFTS).join(format!("{name}.md"))
    }

    fn state(&self) -> BTreeMap<String, Record> {
        std::fs::read(self.dir.join(STATE_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn save_state(&self, state: &BTreeMap<String, Record>) -> std::io::Result<()> {
        let text = serde_json::to_string_pretty(state).unwrap_or_else(|_| "{}".into());
        write_atomic(&self.dir.join(STATE_FILE), &text)
    }

    fn read(path: &Path) -> Option<String> {
        let meta = std::fs::metadata(path).ok()?;
        if !meta.is_file() || meta.len() > 64 * 1024 {
            return None;
        }
        Some(String::from_utf8_lossy(&std::fs::read(path).ok()?).into_owned())
    }

    fn names_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter_map(|f| f.strip_suffix(".md").map(str::to_string))
            .filter(|n| valid_name(n))
            .collect();
        names.sort();
        names
    }

    /// Every skill and draft, live ones first.
    pub fn list(&self) -> Vec<Entry> {
        let state = self.state();
        let mut out = Vec::new();
        for name in Self::names_in(&self.dir) {
            let Some(meta) = Self::read(&self.live_path(&name)).and_then(|t| parse_meta(&t)) else {
                continue;
            };
            let record = state.get(&name);
            out.push(Entry {
                meta: Meta {
                    name: name.clone(),
                    ..meta
                },
                status: Some(record.map_or(Status::Trial, |r| r.status)),
                outcomes: record.map(|r| r.outcomes.clone()).unwrap_or_default(),
                draft: false,
                has_previous: self.prev_path(&name).is_file(),
            });
        }
        for name in Self::names_in(&self.dir.join(DRAFTS)) {
            if let Some(meta) = Self::read(&self.draft_path(&name)).and_then(|t| parse_meta(&t)) {
                out.push(Entry {
                    meta: Meta { name, ..meta },
                    status: None,
                    outcomes: Vec::new(),
                    draft: true,
                    has_previous: false,
                });
            }
        }
        out
    }

    /// The live text and the pending draft of `name`.
    pub fn get(&self, name: &str) -> (Option<String>, Option<String>) {
        if !valid_name(name) {
            return (None, None);
        }
        (
            Self::read(&self.live_path(name)),
            Self::read(&self.draft_path(name)),
        )
    }

    pub fn status(&self, name: &str) -> Option<Record> {
        self.state().get(name).cloned()
    }

    /// Store a validated draft. A draft of the same kind (`updates:`) is
    /// filed under the skill it rewrites, so accepting it replaces that one.
    pub fn draft(&self, text: &str) -> Result<Value, String> {
        let meta = validate(text)?;
        let target = meta.updates.clone().unwrap_or_else(|| meta.name.clone());
        let text = if target != meta.name {
            // Accepting must land under the old name the runs already know.
            text.replacen(
                &format!("name: {}", meta.name),
                &format!("name: {target}"),
                1,
            )
        } else {
            text.to_string()
        };
        write_atomic(&self.draft_path(&target), text.trim_end())
            .map_err(|e| format!("the draft could not be saved: {e}"))?;
        let replaces = self.live_path(&target).is_file();
        Ok(json!({
            "ok": true,
            "name": target,
            "status": "draft",
            "replaces": replaces,
        }))
    }

    /// Put the draft into use (on trial). The live text it replaces becomes
    /// the previous version.
    pub fn accept(&self, name: &str) -> Result<Value, String> {
        let draft = Self::read(&self.draft_path(name)).ok_or(format!("no draft named {name}"))?;
        validate(&draft)?;
        let live = self.live_path(name);
        if let Some(old) = Self::read(&live) {
            write_atomic(&self.prev_path(name), &old).map_err(|e| e.to_string())?;
        }
        write_atomic(&live, &draft).map_err(|e| e.to_string())?;
        remove_if_present(&self.draft_path(name)).map_err(|e| e.to_string())?;
        let mut state = self.state();
        state.insert(
            name.to_string(),
            Record {
                status: Status::Trial,
                outcomes: Vec::new(),
                accepted_at: now_secs(),
            },
        );
        self.save_state(&state).map_err(|e| e.to_string())?;
        Ok(json!({"ok": true, "name": name, "status": "trial"}))
    }

    pub fn result(&self, name: &str, outcome: Outcome) -> Result<Value, String> {
        if !self.live_path(name).is_file() {
            return Err(format!("no skill named {name} in use"));
        }
        let mut state = self.state();
        let record = state.entry(name.to_string()).or_insert(Record {
            status: Status::Trial,
            outcomes: Vec::new(),
            accepted_at: 0,
        });
        let before = record.status;
        record.record(outcome);
        let after = record.status;
        let recent = record.outcomes.clone();
        self.save_state(&state).map_err(|e| e.to_string())?;
        let mut reply =
            json!({"ok": true, "name": name, "status": after.as_str(), "recent": recent});
        if before != Status::Suspended && after == Status::Suspended {
            reply["message"] = json!(format!(
                "{name} has not been working, so it is suspended and later runs will not see it. Tell the user; \
                 they can resume it, restore its previous version, or delete it."
            ));
        }
        Ok(reply)
    }

    pub fn set_suspended(&self, name: &str, suspended: bool) -> Result<Value, String> {
        if !self.live_path(name).is_file() {
            return Err(format!("no skill named {name} in use"));
        }
        let mut state = self.state();
        let record = state.entry(name.to_string()).or_insert(Record {
            status: Status::Trial,
            outcomes: Vec::new(),
            accepted_at: 0,
        });
        record.status = if suspended {
            Status::Suspended
        } else {
            Status::Trial
        };
        if !suspended {
            record.outcomes.clear();
        }
        let status = record.status;
        self.save_state(&state).map_err(|e| e.to_string())?;
        Ok(json!({"ok": true, "name": name, "status": status.as_str()}))
    }

    /// Swap the live text with the previous version; back on trial.
    pub fn restore(&self, name: &str) -> Result<Value, String> {
        let prev =
            Self::read(&self.prev_path(name)).ok_or(format!("{name} has no previous version"))?;
        let live = self.live_path(name);
        let current = Self::read(&live);
        write_atomic(&live, &prev).map_err(|e| e.to_string())?;
        match current {
            Some(current) => write_atomic(&self.prev_path(name), &current),
            None => remove_if_present(&self.prev_path(name)),
        }
        .map_err(|e| e.to_string())?;
        let mut state = self.state();
        state.insert(
            name.to_string(),
            Record {
                status: Status::Trial,
                outcomes: Vec::new(),
                accepted_at: now_secs(),
            },
        );
        self.save_state(&state).map_err(|e| e.to_string())?;
        Ok(json!({"ok": true, "name": name, "status": "trial", "restored": "previous"}))
    }

    /// Delete the draft only (`draft_only`), or the skill with its draft
    /// and previous version.
    pub fn delete(&self, name: &str, draft_only: bool) -> Result<Value, String> {
        let had_draft = self.draft_path(name).is_file();
        remove_if_present(&self.draft_path(name)).map_err(|e| e.to_string())?;
        if draft_only {
            return if had_draft {
                Ok(json!({"ok": true, "name": name, "deleted": "draft"}))
            } else {
                Err(format!("no draft named {name}"))
            };
        }
        let had_live = self.live_path(name).is_file();
        remove_if_present(&self.live_path(name)).map_err(|e| e.to_string())?;
        remove_if_present(&self.prev_path(name)).map_err(|e| e.to_string())?;
        let mut state = self.state();
        if state.remove(name).is_some() {
            self.save_state(&state).map_err(|e| e.to_string())?;
        }
        if !had_live && !had_draft {
            return Err(format!("no skill or draft named {name}"));
        }
        Ok(json!({"ok": true, "name": name, "deleted": "skill"}))
    }

    /// Skills in use (trial or active) for `bundle`, and the drafts waiting.
    pub fn for_app(&self, bundle: &str) -> (Vec<Entry>, usize) {
        let all = self.list();
        let uses = |e: &Entry| e.meta.apps.iter().any(|a| a == bundle);
        let drafts = all.iter().filter(|e| e.draft && uses(e)).count();
        let live = all
            .into_iter()
            .filter(|e| !e.draft && e.status != Some(Status::Suspended) && uses(&e))
            .collect();
        (live, drafts)
    }
}

/// Add the app's learned skills to its `registry` block.
pub fn with_skills(mut block: Value, store: &Store) -> Value {
    let Some(bundle) = block
        .get("bundle")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return block;
    };
    let (live, drafts) = store.for_app(&bundle);
    if !live.is_empty() {
        block["skills"] = Value::Array(live.iter().take(IN_BLOCK).map(Entry::to_json).collect());
        block["skills_hint"] = json!(
            "Skills learned from earlier runs of tasks in this app. If one matches the task, read it first \
             (GET /agent/skills?name=…, MCP: phone_skills op=read) and follow it; when done, report how it went \
             (op=result: verified / failed / unreviewed)."
        );
    }
    if drafts > 0 {
        block["skill_drafts_waiting"] = json!(drafts);
    }
    block
}

/// Steps in one app before a session is worth learning from.
pub const LEARN_MIN_STEPS: usize = 5;

/// The `learn` block for the release reply: what to report for skills the
/// session followed, and whether to draft one from what it did.
pub fn learn_block(store: &Store, apps: &[(String, usize)], followed: &[String]) -> Option<Value> {
    let mut block = json!({});
    let followed: Vec<&String> = followed
        .iter()
        .filter(|n| store.live_path(n).is_file())
        .collect();
    if !followed.is_empty() {
        block["report"] = json!(followed);
        block["report_message"] = json!(
            "You read these skills this session. For each one you followed, report how the task went: \
             POST /agent/skills {\"op\":\"result\",\"name\":…,\"result\":\"verified|failed|unreviewed\"} \
             (MCP: phone_skills op=result). verified = done and the end screen showed it; failed = not done or \
             only partly; unreviewed = stopped for reasons unrelated to the skill."
        );
    }
    let worth: Vec<&(String, usize)> = apps
        .iter()
        .filter(|(_, steps)| *steps >= LEARN_MIN_STEPS)
        .collect();
    if let Some((bundle, steps)) = worth.first().filter(|_| followed.is_empty()) {
        let existing: Vec<Value> = store
            .list()
            .into_iter()
            .filter(|e| !e.draft && e.meta.apps.iter().any(|a| a == bundle))
            .map(|e| json!({"name": e.meta.name, "description": e.meta.description}))
            .collect();
        block["draft"] = json!({"app": bundle, "steps": steps, "existing": existing});
        block["draft_message"] = json!(format!(
            "This session drove {bundle} for {steps} steps. Only if the task got done and you checked the end \
             screen, write it down as a skill so the next run is faster: markdown with front matter \
             (name: lowercase-hyphen, display, description, apps: [{bundle}], aliases, and updates: <name> when it is \
             the same kind of task as an existing skill listed here) and five sections — ## 入口, ## 步骤, \
             ## 容易出错的地方, ## 怎样算办成, ## 什么时候停下交给用户 — under {MAX_CHARS} characters. Write the general way \
             along the route that worked; detours go under pitfalls; this run's contacts, amounts, text and dates \
             become <参数>; no passwords, codes or personal data. Save it with POST /agent/skills \
             {{\"op\":\"draft\",\"text\":…}} (MCP: phone_skills op=draft). It stays a draft: tell the user what was \
             learned and accept it (op=accept) only after they agree."
        ));
    }
    (block.as_object().is_some_and(|o| !o.is_empty())).then_some(block)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(name: &str, extra: &str) -> String {
        format!(
            "---\nname: {name}\ndisplay: 发微信\ndescription: 给用户指定的联系人发一条微信\napps: [com.tencent.xin]\naliases: [发微信]\n{extra}---\n\
             ## 入口\n打开微信。\n## 步骤\n1. 搜索 <联系人>。\n## 容易出错的地方\n搜索框要先点开。\n## 怎样算办成\n聊天里出现刚发的消息。\n## 什么时候停下交给用户\n要登录时。\n"
        )
    }

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().join("skills"));
        (dir, store)
    }

    #[test]
    fn validate_checks_front_matter_sections_and_personal_data() {
        assert!(validate(&skill("send-wechat", "")).is_ok());
        assert!(validate(&skill("Send_Wechat", ""))
            .unwrap_err()
            .contains("name"));
        assert!(validate("## 入口\nx").unwrap_err().contains("front matter"));
        let missing = skill("send-wechat", "").replace("## 容易出错的地方", "## 其他");
        assert!(validate(&missing).unwrap_err().contains("pitfalls"));
        let english = "---\nname: a\ndescription: d\napps: [com.a.b]\n---\n## Entry\n## Steps\n## Pitfalls\n## Done when\n## Hand over\n";
        assert!(validate(english).is_ok());
        let phone = skill("send-wechat", "").replace("<联系人>", "138 1234 5678 9");
        assert!(validate(&phone).unwrap_err().contains("long number"));
        let mail = skill("send-wechat", "").replace("<联系人>", "me@example.com");
        assert!(validate(&mail).unwrap_err().contains("e-mail"));
        assert!(validate(&"x".repeat(MAX_CHARS + 1))
            .unwrap_err()
            .contains("characters"));
    }

    #[test]
    fn trial_turns_active_on_success_and_suspends_on_failure() {
        let mut r = Record {
            status: Status::Trial,
            outcomes: vec![],
            accepted_at: 0,
        };
        r.record(Outcome::Unreviewed);
        assert_eq!(r.status, Status::Trial);
        r.record(Outcome::Verified);
        assert_eq!(r.status, Status::Active);
        r.record(Outcome::Failed);
        assert_eq!(r.status, Status::Active);
        r.record(Outcome::Unreviewed);
        r.record(Outcome::Failed);
        assert_eq!(r.status, Status::Suspended);

        let mut t = Record {
            status: Status::Trial,
            outcomes: vec![],
            accepted_at: 0,
        };
        t.record(Outcome::Failed);
        assert_eq!(t.status, Status::Suspended);
        for _ in 0..9 {
            t.record(Outcome::Unreviewed);
        }
        assert_eq!(t.outcomes.len(), KEEP);
    }

    #[test]
    fn draft_accept_list_and_registry_block() {
        let (_tmp, store) = store();
        let reply = store.draft(&skill("send-wechat", "")).unwrap();
        assert_eq!(reply["replaces"], false);
        // Drafts are not shown to later runs.
        let block = with_skills(json!({"bundle": "com.tencent.xin"}), &store);
        assert!(block.get("skills").is_none());
        assert_eq!(block["skill_drafts_waiting"], 1);

        store.accept("send-wechat").unwrap();
        let block = with_skills(json!({"bundle": "com.tencent.xin"}), &store);
        assert_eq!(block["skills"][0]["name"], "send-wechat");
        assert_eq!(block["skills"][0]["status"], "trial");
        assert!(block.get("skill_drafts_waiting").is_none());
        assert!(with_skills(json!({"bundle": "com.other"}), &store)
            .get("skills")
            .is_none());

        store.result("send-wechat", Outcome::Failed).unwrap();
        let block = with_skills(json!({"bundle": "com.tencent.xin"}), &store);
        assert!(block.get("skills").is_none(), "suspended skills are hidden");
        store.set_suspended("send-wechat", false).unwrap();
        assert_eq!(store.status("send-wechat").unwrap().status, Status::Trial);
    }

    #[test]
    fn a_rewrite_replaces_on_accept_and_restore_brings_the_old_one_back() {
        let (_tmp, store) = store();
        store.draft(&skill("send-wechat", "")).unwrap();
        store.accept("send-wechat").unwrap();
        let v2 = skill("wechat-message", "updates: send-wechat\n")
            .replace("打开微信。", "从桌面打开微信。");
        let reply = store.draft(&v2).unwrap();
        assert_eq!(reply["name"], "send-wechat");
        assert_eq!(reply["replaces"], true);
        // Until accepted, runs keep the old text.
        assert!(store.get("send-wechat").0.unwrap().contains("\n打开微信。"));
        store.accept("send-wechat").unwrap();
        let (live, draft) = store.get("send-wechat");
        assert!(live.unwrap().contains("从桌面打开微信"));
        assert!(draft.is_none());
        assert_eq!(store.list().len(), 1);
        assert!(store.list()[0].has_previous);

        store.restore("send-wechat").unwrap();
        assert!(store.get("send-wechat").0.unwrap().contains("\n打开微信。"));
        store.delete("send-wechat", false).unwrap();
        assert!(store.list().is_empty());
        assert!(store.delete("send-wechat", false).is_err());
    }

    #[test]
    fn learn_block_asks_for_a_report_or_a_draft() {
        let (_tmp, store) = store();
        assert!(learn_block(&store, &[("com.tencent.xin".into(), 2)], &[]).is_none());
        let block = learn_block(&store, &[("com.tencent.xin".into(), 7)], &[]).unwrap();
        assert_eq!(block["draft"]["app"], "com.tencent.xin");
        assert!(block.get("report").is_none());

        store.draft(&skill("send-wechat", "")).unwrap();
        store.accept("send-wechat").unwrap();
        let block = learn_block(
            &store,
            &[("com.tencent.xin".into(), 7)],
            &["send-wechat".into()],
        )
        .unwrap();
        assert_eq!(block["report"][0], "send-wechat");
        assert!(
            block.get("draft").is_none(),
            "a run that followed a skill reports, it does not re-draft"
        );
        let block = learn_block(&store, &[("com.tencent.xin".into(), 7)], &[]).unwrap();
        assert_eq!(block["draft"]["existing"][0]["name"], "send-wechat");
    }
}
