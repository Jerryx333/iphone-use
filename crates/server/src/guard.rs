//! Owner-set limits on what a driving agent may do, enforced by the daemon so
//! they hold whatever the agent's own prompt says.
//!
//! - `IPHONE_USE_BLOCKED_APPS`: comma-separated bundle ids or app names
//!   ("Wallet, com.apple.Passwords"). An agent cannot open them, and while one
//!   is in front it can only leave (`home`, `launch_app` of another app).
//! - `IPHONE_USE_MAX_ACTIONS`: actions per run and caller. A run is the span
//!   between `POST /agent/run {"action":"start"}` calls, or, without them, one
//!   that ends after [`crate::metrics::IDLE_GAP_MS`] without an action.
//! - `IPHONE_USE_ALLOW_URL_SCHEMES`: comma-separated URL schemes an
//!   `open_url` action may open besides `http`/`https` ("tel, weixin", or
//!   `*` for any). Without it every other scheme is refused: `tel:` dials,
//!   `sms:`/`mailto:` compose, and a custom scheme drives another app.
//!
//! The first two are off by default. A refusal is `403` with `outcome: not_sent`: no
//! input reached the phone. The human control page is never limited.
//!
//! Always on, with no setting: a press that pays or places an order (a
//! control labelled 付款, 立即购买, 转账, 充值, 免密, Pay, Checkout, … — the list
//! `iphone_use_mcp::task` keeps) and typing into a password or verification
//! code field are refused as `sensitive_action`; the owner does those on the
//! phone. Sending, deleting and sharing pass. A saved flow's replay passes
//! too, but only with the daemon's own one-time grant for that exact batch
//! (`crate::flow_grants`); `X-Phone-Flow-Run` alone only marks a replay.
//! So does the one press a daemon task's owner allowed on the phone
//! ([`owner_allowed`]).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use serde_json::Value;

use crate::app_lookup::{catalog, normalize, system_app_bundle, valid_bundle};
use crate::runner::ElementRow;

/// Callers tracked at once; past this the map is cleared.
const MAX_OWNERS: usize = 64;

/// One blocked app: the bundle compared against, and the name shown.
#[derive(Debug, Clone, PartialEq)]
pub struct Blocked {
    pub bundle: String,
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Policy {
    pub blocked: Vec<Blocked>,
    /// `None`: no budget.
    pub max_actions: Option<u32>,
    /// Lowercased schemes `open_url` may open besides `http`/`https`.
    pub allowed_url_schemes: Vec<String>,
    /// `IPHONE_USE_ALLOW_URL_SCHEMES=*`: any scheme.
    pub allow_any_url_scheme: bool,
}

/// Longest URL `open_url` accepts, in bytes.
pub const MAX_URL_LEN: usize = 2048;

/// Schemes `open_url` always opens: they only show a page.
const ALWAYS_ALLOWED_SCHEMES: &[&str] = &["http", "https"];

/// The scheme of an `open_url` URL (lowercased) and its host, if it has one.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedUrl {
    pub scheme: String,
    pub host: Option<String>,
}

/// Parse `url` far enough to apply the scheme policy: an RFC 3986 scheme,
/// no whitespace or control characters, at most [`MAX_URL_LEN`] bytes, and a
/// host for `http`/`https`. The error says what is wrong.
pub fn parse_url(url: &str) -> Result<ParsedUrl, &'static str> {
    if url.is_empty() || url.len() > MAX_URL_LEN {
        return Err("url must be 1 to 2048 bytes");
    }
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("url must not contain spaces or control characters (percent-encode them)");
    }
    let Some((scheme, rest)) = url.split_once(':') else {
        return Err("url needs a scheme, such as https://example.com");
    };
    let mut chars = scheme.chars();
    let valid_scheme = chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    if !valid_scheme {
        return Err("url needs a scheme, such as https://example.com");
    }
    let scheme = scheme.to_ascii_lowercase();
    let host = rest.strip_prefix("//").and_then(|authority| {
        let authority = authority.split(['/', '?', '#']).next().unwrap_or_default();
        let host = authority.rsplit('@').next().unwrap_or_default();
        let host = if let Some(bracketed) = host.strip_prefix('[') {
            bracketed.split(']').next().unwrap_or_default()
        } else {
            host.split(':').next().unwrap_or_default()
        };
        (!host.is_empty()).then(|| host.to_ascii_lowercase())
    });
    if ALWAYS_ALLOWED_SCHEMES.contains(&scheme.as_str()) && host.is_none() {
        return Err("an http(s) url needs a host, such as https://example.com/path");
    }
    Ok(ParsedUrl { scheme, host })
}

/// The `url` of an `open_url` action, if `action` is one.
pub fn open_url_target(action: &Value) -> Option<&str> {
    if action.get("type").and_then(Value::as_str) != Some("open_url") {
        return None;
    }
    action.get("url").and_then(Value::as_str)
}

/// Why an action was refused.
#[derive(Debug, Clone, PartialEq)]
pub enum Refusal {
    OwnerAppProtected,
    /// `launch_app` of a blocked app.
    BlockedLaunch(String),
    /// A blocked app is in front and the action does not leave it.
    BlockedForeground(String),
    /// The foreground could not be read, so a blocked app cannot be ruled out.
    ForegroundUnknown,
    /// The run's budget would be exceeded.
    Budget {
        max: u32,
        used: u32,
        wanted: u32,
    },
    /// `open_url` of a scheme the owner has not allowed.
    UrlScheme(String),
    /// An app install whose bundle id could not be read while the owner
    /// blocks some apps: it cannot be ruled out.
    InstallUnverified,
    /// A press on a control whose words pay or place an order (the label).
    Sensitive(String),
    /// Typing into a password or verification code field (the field's label).
    SensitiveField(String),
    /// A coordinate press in a payment or bank app whose target could not
    /// be read (the app).
    SensitiveUnverified(String),
}

impl Refusal {
    pub fn error(&self) -> &'static str {
        match self {
            Refusal::OwnerAppProtected => "owner_app_protected",
            Refusal::BlockedLaunch(_) | Refusal::BlockedForeground(_) => "blocked_app",
            Refusal::ForegroundUnknown => "foreground_unverified",
            Refusal::Budget { .. } => "action_budget_exhausted",
            Refusal::UrlScheme(_) => "url_scheme_not_allowed",
            Refusal::InstallUnverified => "install_unverified",
            Refusal::Sensitive(_)
            | Refusal::SensitiveField(_)
            | Refusal::SensitiveUnverified(_) => "sensitive_action",
        }
    }

    /// What the agent should do next. An error message is an instruction an
    /// agent follows, so it names the next step.
    pub fn hint(&self) -> String {
        match self {
            Refusal::OwnerAppProtected => "主人确认 App 受保护，请在手机上操作".into(),
            Refusal::BlockedLaunch(name) => format!(
                "{name} is blocked by the phone owner (IPHONE_USE_BLOCKED_APPS). Do not try to reach it another way; tell the user."
            ),
            Refusal::BlockedForeground(name) => format!(
                "{name} is in front and blocked by the phone owner (IPHONE_USE_BLOCKED_APPS). Only leaving is allowed: {{\"type\":\"home\"}} or launch_app of another app."
            ),
            Refusal::ForegroundUnknown => "the app in front could not be read, and the phone owner blocks some apps (IPHONE_USE_BLOCKED_APPS). Nothing was sent; read the screen and retry.".to_string(),
            Refusal::Budget { max, used, wanted } => format!(
                "this run has used {used} of its {max} actions (IPHONE_USE_MAX_ACTIONS) and this request needs {wanted}. Stop and report progress to the user; a new run (POST /agent/run start) gets a fresh budget."
            ),
            Refusal::UrlScheme(scheme) => format!(
                "{scheme}: links are not opened unless the phone owner allows the scheme (IPHONE_USE_ALLOW_URL_SCHEMES, e.g. IPHONE_USE_ALLOW_URL_SCHEMES={scheme}). http and https links always open. Do not reach it another way; tell the user."
            ),
            Refusal::InstallUnverified => "the app's bundle id could not be read from the .app/.ipa, and the phone owner blocks some apps (IPHONE_USE_BLOCKED_APPS), so it is not installed. Tell the user.".to_string(),
            Refusal::Sensitive(label) => format!(
                "这步要主人自己在手机上点：“{label}”像是付款或下单。Nothing was sent. Do not reach it another way (coordinates, perform, the keyboard, a batch); stop and tell the user to tap it on the phone themselves."
            ),
            Refusal::SensitiveField(label) => format!(
                "这步要主人自己在手机上输入：“{label}”是密码或验证码框。Nothing was typed. Do not type it another way; to sign in use phone_login (POST /agent/login), otherwise tell the user to enter it on the phone."
            ),
            Refusal::SensitiveUnverified(app) => format!(
                "这步要主人自己在手机上点：{app} 是支付或银行类 App，这个坐标下的控件读不出来，没法确认它不是付款。Nothing was sent. Tap by label or element instead, or tell the user."
            ),
        }
    }

    pub fn to_json(&self) -> Value {
        let mut body = serde_json::json!({
            "ok": false,
            "error": self.error(),
            "outcome": "not_sent",
            "retry_safe": matches!(self, Refusal::ForegroundUnknown),
            "hint": self.hint(),
        });
        match self {
            Refusal::BlockedLaunch(name) | Refusal::BlockedForeground(name) => {
                body["app"] = Value::String(name.clone());
            }
            Refusal::Budget { max, used, .. } => {
                body["budget"] = serde_json::json!({"max": max, "used": used});
            }
            Refusal::UrlScheme(scheme) => {
                body["scheme"] = Value::String(scheme.clone());
            }
            Refusal::Sensitive(label) | Refusal::SensitiveField(label) => {
                body["label"] = Value::String(label.clone());
            }
            Refusal::SensitiveUnverified(app) => {
                body["app"] = Value::String(app.clone());
            }
            Refusal::OwnerAppProtected
            | Refusal::ForegroundUnknown
            | Refusal::InstallUnverified => {}
        }
        body
    }
}

impl Policy {
    /// The daemon's policy, read from the environment once.
    pub fn get() -> &'static Policy {
        static POLICY: OnceLock<Policy> = OnceLock::new();
        POLICY.get_or_init(|| Policy::from_map(|key| std::env::var(key).ok()))
    }

    pub fn from_map(get: impl Fn(&str) -> Option<String>) -> Policy {
        let mut blocked: Vec<Blocked> = Vec::new();
        for entry in get("IPHONE_USE_BLOCKED_APPS")
            .unwrap_or_default()
            .split([',', '，', '\n'])
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
        {
            match resolve(entry) {
                Some(app) if !blocked.iter().any(|b| b.bundle == app.bundle) => blocked.push(app),
                Some(_) => {}
                None => tracing::warn!(
                    "IPHONE_USE_BLOCKED_APPS: {entry:?} is neither a bundle id nor a known app name; ignored"
                ),
            }
        }
        let max_actions = get("IPHONE_USE_MAX_ACTIONS")
            .and_then(|raw| raw.trim().parse::<u32>().ok())
            .filter(|max| *max > 0);
        let mut allowed_url_schemes: Vec<String> = Vec::new();
        let mut allow_any_url_scheme = false;
        for entry in get("IPHONE_USE_ALLOW_URL_SCHEMES")
            .unwrap_or_default()
            .split([',', '，', '\n'])
            .map(|entry| entry.trim().trim_end_matches("://").trim_end_matches(':'))
            .filter(|entry| !entry.is_empty())
        {
            if entry == "*" {
                allow_any_url_scheme = true;
                continue;
            }
            let scheme = entry.to_ascii_lowercase();
            if !allowed_url_schemes.contains(&scheme) {
                allowed_url_schemes.push(scheme);
            }
        }
        Policy {
            blocked,
            max_actions,
            allowed_url_schemes,
            allow_any_url_scheme,
        }
    }

    pub fn is_active(&self) -> bool {
        !self.blocked.is_empty() || self.max_actions.is_some()
    }

    fn blocked_bundle(&self, bundle: &str) -> Option<&Blocked> {
        self.blocked
            .iter()
            .find(|b| b.bundle.eq_ignore_ascii_case(bundle))
    }

    /// A `launch_app` of a blocked app (after `app` names are resolved to
    /// bundles), checked without touching the phone.
    pub fn check_launch(&self, action: &Value) -> Result<(), Refusal> {
        match launch_target(action).and_then(|bundle| self.blocked_bundle(&bundle)) {
            Some(app) => Err(Refusal::BlockedLaunch(app.name.clone())),
            None => Ok(()),
        }
    }

    /// An `open_url` whose scheme the owner has not allowed, checked without
    /// touching the phone. Applies with or without the other limits: `http`
    /// and `https` always pass, every other scheme only when listed.
    /// A URL that does not parse is left to the action's validation.
    pub fn check_url(&self, action: &Value) -> Result<(), Refusal> {
        let Some(Ok(parsed)) = open_url_target(action).map(parse_url) else {
            return Ok(());
        };
        if self.allow_any_url_scheme
            || ALWAYS_ALLOWED_SCHEMES.contains(&parsed.scheme.as_str())
            || self.allowed_url_schemes.contains(&parsed.scheme)
        {
            return Ok(());
        }
        Err(Refusal::UrlScheme(parsed.scheme))
    }

    /// An app install of `bundle` (`None`: its bundle id could not be read).
    /// A blocked app is the owner's to put on the phone, not an agent's; an
    /// unreadable bundle cannot be ruled out while anything is blocked.
    pub fn check_install(&self, bundle: Option<&str>) -> Result<(), Refusal> {
        match bundle {
            Some(bundle) => match self.blocked_bundle(bundle) {
                Some(app) => Err(Refusal::BlockedLaunch(app.name.clone())),
                None => Ok(()),
            },
            None if !self.blocked.is_empty() => Err(Refusal::InstallUnverified),
            None => Ok(()),
        }
    }

    /// Whether the foreground must be read before `action`: there is a
    /// blocklist and the action does not leave the current app.
    pub fn needs_foreground(&self, action: &Value) -> bool {
        !self.blocked.is_empty() && !leaves_app(action)
    }

    /// `active`: the bundles iOS reports as active (the foreground app, plus
    /// SpringBoard under a banner).
    pub fn check_foreground(&self, active: &[String]) -> Result<(), Refusal> {
        match active.iter().find_map(|bundle| self.blocked_bundle(bundle)) {
            Some(app) => Err(Refusal::BlockedForeground(app.name.clone())),
            None => Ok(()),
        }
    }
}

/// Bundle id, Apple app name, or catalog name/alias, exactly.
fn resolve(entry: &str) -> Option<Blocked> {
    if let Some(bundle) = system_app_bundle(entry) {
        return Some(Blocked {
            bundle: bundle.to_string(),
            name: entry.to_string(),
        });
    }
    let wanted = normalize(entry);
    if let Some(app) = catalog().iter().find(|app| {
        std::iter::once(&app.name)
            .chain(app.aliases.iter())
            .chain(std::iter::once(&app.bundle_id))
            .any(|name| normalize(name) == wanted)
    }) {
        return Some(Blocked {
            bundle: app.bundle_id.clone(),
            name: app.name.clone(),
        });
    }
    valid_bundle(entry).then(|| Blocked {
        bundle: entry.to_string(),
        name: entry.to_string(),
    })
}

fn launch_target(action: &Value) -> Option<String> {
    if action.get("type").and_then(Value::as_str) != Some("launch_app") {
        return None;
    }
    action
        .get("bundle")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            action
                .get("name")
                .and_then(Value::as_str)
                .and_then(system_app_bundle)
                .map(str::to_string)
        })
}

/// Actions that only leave the app in front. `open_url` hands the link to
/// another app (Safari, or the scheme's owner), so it leaves too.
fn leaves_app(action: &Value) -> bool {
    match action.get("type").and_then(Value::as_str) {
        Some("home" | "launch_app" | "open_url") => true,
        Some("shortcut") => action.get("name").and_then(Value::as_str) == Some("home"),
        _ => false,
    }
}

#[derive(Debug, Clone, Copy)]
struct Spent {
    used: u32,
    last_ms: u64,
}

fn budgets() -> &'static Mutex<HashMap<String, Spent>> {
    static BUDGETS: OnceLock<Mutex<HashMap<String, Spent>>> = OnceLock::new();
    BUDGETS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Charge `wanted` actions to `owner`'s run, or refuse with nothing charged.
/// Charged before the actions run: an action that fails still counts.
pub fn charge(policy: &Policy, owner: &str, wanted: u32, now_ms: u64) -> Result<(), Refusal> {
    let Some(max) = policy.max_actions else {
        return Ok(());
    };
    let mut map = budgets()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if map.len() >= MAX_OWNERS && !map.contains_key(owner) {
        map.clear();
    }
    let spent = map.entry(owner.to_string()).or_insert(Spent {
        used: 0,
        last_ms: now_ms,
    });
    if now_ms.saturating_sub(spent.last_ms) > crate::metrics::IDLE_GAP_MS {
        spent.used = 0;
    }
    if spent.used.saturating_add(wanted) > max {
        return Err(Refusal::Budget {
            max,
            used: spent.used,
            wanted,
        });
    }
    spent.used += wanted;
    spent.last_ms = now_ms;
    Ok(())
}

/// A new run for `owner`: its budget starts over.
pub fn reset(owner: &str) {
    budgets()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(owner);
}

/// Used/max for `owner`, for status displays; `None` without a budget.
pub fn budget_status(policy: &Policy, owner: &str, now_ms: u64) -> Option<Value> {
    let max = policy.max_actions?;
    let used = budgets()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(owner)
        .filter(|spent| now_ms.saturating_sub(spent.last_ms) <= crate::metrics::IDLE_GAP_MS)
        .map_or(0, |spent| spent.used);
    Some(serde_json::json!({"max": max, "used": used}))
}

/// The policy as `/agent/capabilities` reports it.
pub fn describe(policy: &Policy) -> Value {
    serde_json::json!({
        "blocked_apps": policy
            .blocked
            .iter()
            .map(|b| serde_json::json!({"bundle": b.bundle, "name": b.name}))
            .collect::<Vec<_>>(),
        "max_actions_per_run": policy.max_actions,
        "open_url_schemes": if policy.allow_any_url_scheme {
            serde_json::json!("*")
        } else {
            serde_json::json!(ALWAYS_ALLOWED_SCHEMES
                .iter()
                .map(|s| s.to_string())
                .chain(policy.allowed_url_schemes.iter().cloned())
                .collect::<Vec<_>>())
        },
    })
}

// ---------------------------------------------------------------------------
// Sensitive actions
// ---------------------------------------------------------------------------

/// Words on a field that holds a secret. ASCII ones match whole words.
const SECRET_FIELD_WORDS: &[&str] = &[
    "密码",
    "口令",
    "验证码",
    "校验码",
    "动态码",
    "安全码",
    "Password",
    "Passcode",
    "PIN",
    "OTP",
    "CVV",
    "CVC",
    "Verification Code",
    "Security Code",
    "One-Time Code",
];

/// Kinds a person types into.
const TEXT_FIELD_KINDS: &[&str] = &["TextField", "SecureTextField", "SearchField", "TextView"];

/// `perform` verbs that press the control; the rest (increment, adjust,
/// scroll_to_visible, …) move it without activating it.
const PRESSING_PERFORMS: &[&str] = &[
    "toggle",
    "menu",
    "double_tap",
    "two_finger_tap",
    "force_press",
];

/// How far (as a fraction of the screen) a swipe may move and still land
/// as a press.
const PRESS_SLOP: f64 = 0.03;

/// Key names the runner sends as Return (`RunnerClient::named_key`).
const RETURN_KEYS: &[&str] = &["return", "enter", "send", "go", "search"];

/// Pass a paying press the owner just allowed on the phone, through a
/// task's confirmation (`iphone_use_mcp::task::approve_press`): once, and
/// only that control. Every other refusal stands.
pub fn owner_allowed(refusal: Refusal, token: Option<&str>) -> Result<(), Refusal> {
    match &refusal {
        Refusal::Sensitive(label) if iphone_use_mcp::task::take_approved_press(token, label) => {
            Ok(())
        }
        _ => Err(refusal),
    }
}

/// The word in `label` that pays or places an order.
pub fn paying_word(label: &str) -> Option<&'static str> {
    iphone_use_mcp::task::paying_word(label)
}

fn secret_word(text: &str) -> bool {
    let lower = text.to_lowercase();
    SECRET_FIELD_WORDS.iter().any(|word| {
        if !word.is_ascii() {
            text.contains(word)
        } else if word.contains(' ') || word.contains('-') {
            lower.contains(&word.to_lowercase())
        } else {
            lower
                .split(|c: char| !c.is_alphanumeric())
                .any(|token| token == word.to_lowercase())
        }
    })
}

fn refuse_label(label: &str) -> Result<(), Refusal> {
    match paying_word(label) {
        Some(_) => Err(Refusal::Sensitive(label.trim().to_string())),
        None => Ok(()),
    }
}

fn kind(action: &Value) -> &str {
    action.get("type").and_then(Value::as_str).unwrap_or("")
}

/// What the action names in its own fields: a `tap` label, a `tap_locator`
/// label, an `alert` button. Checked before anything is sent, so a batch
/// with one such step is refused whole.
pub fn check_sensitive_request(action: &Value) -> Result<(), Refusal> {
    let field =
        |value: &Value, key: &str| value.get(key).and_then(Value::as_str).map(str::to_string);
    let named: Vec<String> = match kind(action) {
        "tap" | "longpress" => field(action, "label").into_iter().collect(),
        "tap_locator" => action
            .get("locator")
            .map(|locator| {
                ["label", "label_contains", "value"]
                    .iter()
                    .filter_map(|key| field(locator, key))
                    .collect()
            })
            .unwrap_or_default(),
        "alert" => field(action, "button").into_iter().collect(),
        _ => Vec::new(),
    };
    named.iter().try_for_each(|label| refuse_label(label))
}

/// What the sensitive check must read on the phone before `action` runs,
/// beyond [`check_sensitive_request`].
#[derive(Debug, Clone, PartialEq)]
pub enum SensitiveRead {
    /// Nothing: the action presses nothing that could pay.
    None,
    /// The rows a `tap` `label` names (and their contents), of `kind` if given.
    Label { label: String, kind: Option<String> },
    /// The snapshot row it names (`tap`/`perform` press it, `set_value` types into it).
    Element,
    /// Every row the `tap_locator`'s locator could match.
    Locator,
    /// The controls under these normalized points.
    Points(Vec<(f64, f64)>),
    /// The focused field (typing) and, with `submits`, the Return key too.
    Focused { submits: bool },
    /// The buttons of the alert in front (`alert accept`).
    Alert,
}

pub fn sensitive_read(action: &Value) -> SensitiveRead {
    // Only points the action would act on: an invalid one is refused by its
    // own validation, with nothing read or sent.
    let point = |value: &Value| {
        let x = value.get("x").and_then(Value::as_f64)?;
        let y = value.get("y").and_then(Value::as_f64)?;
        ((0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y)).then_some((x, y))
    };
    let field = |key: &str| action.get(key).and_then(Value::as_str).map(str::to_string);
    match kind(action) {
        "tap" if action.get("label").is_some() => match field("label") {
            Some(label) if !label.is_empty() => SensitiveRead::Label {
                label,
                kind: field("kind")
                    .map(|kind| kind.trim_start_matches("XCUIElementType").to_string())
                    .filter(|kind| !kind.is_empty()),
            },
            _ => SensitiveRead::None,
        },
        "tap" if action.get("element").is_some() => SensitiveRead::Element,
        "set_value" => SensitiveRead::Element,
        "perform" => {
            let verb = action.get("action").and_then(Value::as_str).unwrap_or("");
            if PRESSING_PERFORMS.contains(&verb) {
                SensitiveRead::Element
            } else {
                SensitiveRead::None
            }
        }
        "longpress"
            if action.get("fingers").is_some_and(|f| f.as_u64() != Some(1))
                || action.get("points").is_some() =>
        {
            SensitiveRead::None
        }
        "tap" | "longpress" => {
            point(action).map_or(SensitiveRead::None, |p| SensitiveRead::Points(vec![p]))
        }
        "multi_longpress" => {
            let all = action.get("points").and_then(Value::as_array);
            let points: Vec<(f64, f64)> = all
                .map(|points| points.iter().filter_map(point).collect())
                .unwrap_or_default();
            if (2..=5).contains(&points.len()) && all.is_some_and(|all| all.len() == points.len()) {
                SensitiveRead::Points(points)
            } else {
                SensitiveRead::None
            }
        }
        "tap_locator" => SensitiveRead::Locator,
        // A swipe that barely moves is a press where it starts and ends.
        "swipe" | "drag" => {
            let ends: Vec<(f64, f64)> = match action.get("points").and_then(Value::as_array) {
                Some(points) => points.iter().filter_map(point).collect(),
                None => [("x1", "y1"), ("x2", "y2")]
                    .iter()
                    .filter_map(|(x, y)| {
                        Some((
                            action.get(*x).and_then(Value::as_f64)?,
                            action.get(*y).and_then(Value::as_f64)?,
                        ))
                    })
                    .collect(),
            };
            let still = ends
                .iter()
                .all(|&(x, y)| point(&serde_json::json!({"x": x, "y": y})).is_some())
                && ends.first().is_some_and(|&(x0, y0)| {
                    ends.iter()
                        .all(|&(x, y)| (x - x0).abs() < PRESS_SLOP && (y - y0).abs() < PRESS_SLOP)
                });
            if still {
                SensitiveRead::Points(ends)
            } else {
                SensitiveRead::None
            }
        }
        // A continuation types text the request does not carry: it may
        // hold a newline.
        "text" => SensitiveRead::Focused {
            submits: action.get("continue_token").is_some()
                || action
                    .get("text")
                    .and_then(Value::as_str)
                    .is_some_and(|text| text.contains(['\n', '\r'])),
        },
        "key" => {
            let name = action.get("name").and_then(Value::as_str).unwrap_or("");
            if RETURN_KEYS.contains(&name.to_ascii_lowercase().as_str()) {
                SensitiveRead::Focused { submits: true }
            } else {
                SensitiveRead::None
            }
        }
        "alert"
            if action.get("button").is_none()
                && action.get("action").and_then(Value::as_str) == Some("accept") =>
        {
            SensitiveRead::Alert
        }
        _ => SensitiveRead::None,
    }
}

/// One past the last row of `index`'s subtree (rows are in document order).
fn subtree_end(rows: &[ElementRow], index: usize) -> usize {
    let depth = rows[index].depth;
    rows[index + 1..]
        .iter()
        .position(|row| row.depth <= depth)
        .map_or(rows.len(), |offset| index + 1 + offset)
}

fn row_words(row: &ElementRow) -> impl Iterator<Item = &str> {
    std::iter::once(row.label.as_str())
        .chain(row.identifier.as_deref())
        .filter(|text| !text.trim().is_empty())
}

/// A press on row `index`: its own words and those of everything inside it
/// (a Cell whose StaticText says 立即购买).
pub fn check_pressed_row(rows: &[ElementRow], index: usize) -> Result<(), Refusal> {
    if index >= rows.len() {
        return Ok(());
    }
    match pressed_labels(rows, index).into_iter().next() {
        Some(label) => Err(Refusal::Sensitive(label)),
        None => Ok(()),
    }
}

fn pressed_labels(rows: &[ElementRow], index: usize) -> Vec<String> {
    // Unlabelled containers are not rows, so depth alone can make a later
    // sibling's child look like this row's: inside means drawn inside too.
    let [x0, y0, w0, h0] = rows[index].rect;
    let inside = |row: &ElementRow| {
        let [x, y, w, h] = row.rect;
        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
        (x0..=x0 + w0).contains(&cx) && (y0..=y0 + h0).contains(&cy)
    };
    let end = subtree_end(rows, index);
    let mut labels = Vec::new();
    for row in
        std::iter::once(&rows[index]).chain(rows[index + 1..end].iter().filter(|row| inside(row)))
    {
        for label in row_words(row).filter(|text| paying_word(text).is_some()) {
            let label = label.trim().to_owned();
            if !labels.contains(&label) {
                labels.push(label);
            }
        }
    }
    labels
}

/// A `tap` by `label`: every row it could name, with what is inside it (a
/// Cell named 商品详情 whose middle is a 立即购买 button).
pub fn check_label_rows(
    rows: &[ElementRow],
    label: &str,
    kind: Option<&str>,
) -> Result<(), Refusal> {
    for (index, row) in rows.iter().enumerate() {
        if row.label == label && kind.is_none_or(|kind| row.kind == kind) {
            check_pressed_row(rows, index)?;
        }
    }
    Ok(())
}

/// Typing into `row`: a secure field, or one whose label, placeholder or
/// identifier asks for a password or a verification code.
pub fn check_typed_row(row: &ElementRow) -> Result<(), Refusal> {
    if !TEXT_FIELD_KINDS.contains(&row.kind.as_str()) {
        return Ok(());
    }
    let words: Vec<&str> = row_words(row).chain(row.placeholder.as_deref()).collect();
    if row.kind == "SecureTextField" || words.iter().any(|text| secret_word(text)) {
        let name = words.first().copied().unwrap_or(row.kind.as_str());
        return Err(Refusal::SensitiveField(name.trim().to_string()));
    }
    Ok(())
}

/// Kinds a press activates, as opposed to text and containers.
const CONTROL_KINDS: &[&str] = &[
    "Button",
    "Cell",
    "Link",
    "Key",
    "Switch",
    "Toggle",
    "Tab",
    "Icon",
    "MenuItem",
    "SegmentedControl",
    "Image",
];

/// The press at runner point (`x`, `y`): the smallest control that holds it
/// (with everything inside it), and the smallest labelled row of any kind
/// (text drawn on a bare view). `None`: no words there, so the target is
/// unknown.
pub fn check_point(rows: &[ElementRow], x: f64, y: f64) -> Option<Result<(), Refusal>> {
    point_sensitive_labels(rows, x, y).map(|labels| match labels.into_iter().next() {
        Some(label) => Err(Refusal::Sensitive(label)),
        None => Ok(()),
    })
}

/// All sensitive words in the actual hit control, including its children.
pub fn point_sensitive_labels(rows: &[ElementRow], x: f64, y: f64) -> Option<Vec<String>> {
    let (control, text) = point_rows(rows, x, y);
    let control_words = control.is_some_and(|index| {
        rows[index..subtree_end(rows, index)]
            .iter()
            .any(|row| row_words(row).next().is_some())
    });
    if !control_words && text.is_none() {
        return None;
    }
    let mut labels = control.map(|i| pressed_labels(rows, i)).unwrap_or_default();
    if let Some(index) = text {
        for label in row_words(&rows[index]).filter(|t| paying_word(t).is_some()) {
            let label = label.trim().to_owned();
            if !labels.contains(&label) {
                labels.push(label);
            }
        }
    }
    Some(labels)
}

/// The exact rows used by the coordinate guard (control and text hit).
pub(crate) fn point_rows(rows: &[ElementRow], x: f64, y: f64) -> (Option<usize>, Option<usize>) {
    let holds = |row: &ElementRow| {
        let [rx, ry, rw, rh] = row.rect;
        row.visible != Some(false)
            && !matches!(row.kind.as_str(), "Application" | "Window")
            && rw > 0.0
            && rh > 0.0
            && x >= rx
            && x <= rx + rw
            && y >= ry
            && y <= ry + rh
    };
    let smallest = |keep: &dyn Fn(&ElementRow) -> bool| {
        rows.iter()
            .enumerate()
            .filter(|(_, row)| holds(row) && keep(row))
            .min_by(|a, b| (a.1.rect[2] * a.1.rect[3]).total_cmp(&(b.1.rect[2] * b.1.rect[3])))
            .map(|(index, _)| index)
    };
    let control = smallest(&|row| CONTROL_KINDS.contains(&row.kind.as_str()));
    let text = smallest(&|row| row_words(row).next().is_some());
    (control, text)
}

/// The focused text field, if the tree marks one.
pub fn focused_field(rows: &[ElementRow]) -> Option<&ElementRow> {
    rows.iter()
        .find(|row| row.focused == Some(true) && TEXT_FIELD_KINDS.contains(&row.kind.as_str()))
}

/// Return pressed in the focused field: the Return key's own words (a
/// keyboard whose action key says 付款) and the field's. An unlabelled
/// keyboard is not a row, so the keyboard is the box around its `Key` rows;
/// its action key is a `Button` inside that box.
pub fn check_return(rows: &[ElementRow]) -> Result<(), Refusal> {
    let mut keyboard: Option<[f64; 4]> = None;
    for row in rows
        .iter()
        .filter(|row| row.kind == "Key" || row.kind == "Keyboard")
    {
        let [x, y, w, h] = row.rect;
        keyboard = Some(match keyboard {
            None => [x, y, x + w, y + h],
            Some([x0, y0, x1, y1]) => [x0.min(x), y0.min(y), x1.max(x + w), y1.max(y + h)],
        });
    }
    if let Some([x0, y0, x1, y1]) = keyboard {
        for (index, row) in rows.iter().enumerate() {
            let [x, y, w, h] = row.rect;
            let (cx, cy) = (x + w / 2.0, y + h / 2.0);
            if matches!(row.kind.as_str(), "Button" | "Key" | "Keyboard")
                && (x0..=x1).contains(&cx)
                && (y0..=y1).contains(&cy)
            {
                check_pressed_row(rows, index)?;
            }
        }
    }
    if let Some(field) = focused_field(rows) {
        if let Some(label) = row_words(field).find(|text| paying_word(text).is_some()) {
            return Err(Refusal::Sensitive(label.trim().to_string()));
        }
    }
    Ok(())
}

/// `alert accept` presses the alert's default button: any of its buttons
/// that pays refuses it.
pub fn check_alert_buttons(buttons: &[String]) -> Result<(), Refusal> {
    buttons.iter().try_for_each(|button| refuse_label(button))
}

/// Payment and bank apps: where a press whose target cannot be read is
/// refused. Names from the app catalog, plus bundle words.
pub fn payment_app(active: &[String]) -> Option<String> {
    const NAME_WORDS: &[&str] = &[
        "银行",
        "支付",
        "钱包",
        "信用卡",
        "证券",
        "Bank",
        "Pay",
        "Wallet",
    ];
    const BUNDLE_WORDS: &[&str] = &["bank", "alipay", "unionpay", "paypal", "wallet", "passbook"];
    active.iter().find_map(|bundle| {
        let catalogued = catalog()
            .iter()
            .find(|app| app.bundle_id.eq_ignore_ascii_case(bundle));
        if let Some(app) = catalogued {
            if std::iter::once(&app.name)
                .chain(app.aliases.iter())
                .any(|name| NAME_WORDS.iter().any(|word| name.contains(word)))
            {
                return Some(app.name.clone());
            }
        }
        let lower = bundle.to_ascii_lowercase();
        lower
            .split('.')
            .any(|part| {
                BUNDLE_WORDS.iter().any(|word| part.contains(word)) || part.ends_with("pay")
            })
            .then(|| catalogued.map_or_else(|| bundle.clone(), |app| app.name.clone()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn policy(blocked: &str, max: &str) -> Policy {
        Policy::from_map(|key| match key {
            "IPHONE_USE_BLOCKED_APPS" => Some(blocked.to_string()),
            "IPHONE_USE_MAX_ACTIONS" => Some(max.to_string()),
            _ => None,
        })
    }

    #[test]
    fn default_policy_is_off() {
        let p = Policy::from_map(|_| None);
        assert!(!p.is_active());
        assert!(p
            .check_launch(&json!({"type":"launch_app","bundle":"com.apple.Passbook"}))
            .is_ok());
        assert!(!p.needs_foreground(&json!({"type":"tap","x":1,"y":1})));
        assert!(charge(&p, "default-off", 10_000, 0).is_ok());
    }

    #[test]
    fn entries_resolve_by_bundle_apple_name_and_catalog_name() {
        let p = policy("com.example.bank, Wallet，微信, not a thing", "0");
        let bundles: Vec<_> = p.blocked.iter().map(|b| b.bundle.as_str()).collect();
        assert_eq!(bundles[0], "com.example.bank");
        assert_eq!(bundles[1], system_app_bundle("Wallet").unwrap());
        assert_eq!(bundles[2], "com.tencent.xin");
        assert_eq!(bundles.len(), 3, "unknown names are dropped: {bundles:?}");
        assert_eq!(p.max_actions, None, "0 means no budget");
    }

    #[test]
    fn launch_of_a_blocked_app_is_refused_by_bundle_or_system_name() {
        let p = policy("Wallet", "");
        let wallet = system_app_bundle("Wallet").unwrap();
        let refused = p.check_launch(&json!({"type":"launch_app","bundle":wallet}));
        assert!(matches!(refused, Err(Refusal::BlockedLaunch(ref name)) if name == "Wallet"));
        assert!(p
            .check_launch(&json!({"type":"launch_app","name":"Wallet"}))
            .is_err());
        assert!(p
            .check_launch(&json!({"type":"launch_app","bundle":"com.apple.Preferences"}))
            .is_ok());
        assert!(p.check_launch(&json!({"type":"tap","x":1,"y":1})).is_ok());
    }

    #[test]
    fn inside_a_blocked_app_only_leaving_is_allowed() {
        let p = policy("com.example.bank", "");
        assert!(p.needs_foreground(&json!({"type":"tap","x":1,"y":1})));
        assert!(p.needs_foreground(&json!({"type":"shortcut","name":"spotlight"})));
        assert!(!p.needs_foreground(&json!({"type":"home"})));
        assert!(!p.needs_foreground(&json!({"type":"shortcut","name":"home"})));
        assert!(!p.needs_foreground(&json!({"type":"launch_app","bundle":"com.apple.Preferences"})));
        let refused =
            p.check_foreground(&["com.apple.springboard".into(), "com.example.bank".into()]);
        assert_eq!(refused.unwrap_err().error(), "blocked_app");
        assert!(p
            .check_foreground(&["com.apple.Preferences".into()])
            .is_ok());
    }

    #[test]
    fn budget_refuses_past_the_limit_and_restarts_after_idle_or_reset() {
        let p = policy("", "5");
        let owner = "budget-test";
        assert!(charge(&p, owner, 3, 1_000).is_ok());
        let refused = charge(&p, owner, 3, 2_000).unwrap_err();
        assert_eq!(
            refused,
            Refusal::Budget {
                max: 5,
                used: 3,
                wanted: 3
            }
        );
        assert_eq!(refused.to_json()["outcome"], "not_sent");
        assert!(charge(&p, owner, 2, 3_000).is_ok());
        assert!(charge(&p, owner, 1, 4_000).is_err());
        assert_eq!(budget_status(&p, owner, 4_000).unwrap()["used"], 5);
        // A long pause is a new run.
        assert!(charge(&p, owner, 5, 4_000 + crate::metrics::IDLE_GAP_MS).is_ok());
        reset(owner);
        assert_eq!(budget_status(&p, owner, 0).unwrap()["used"], 0);
        assert!(charge(&p, owner, 5, 10).is_ok());
    }

    #[test]
    fn budgets_are_per_owner() {
        let p = policy("", "1");
        assert!(charge(&p, "owner-a-per", 1, 0).is_ok());
        assert!(charge(&p, "owner-b-per", 1, 0).is_ok());
        assert!(charge(&p, "owner-a-per", 1, 0).is_err());
    }

    #[test]
    fn urls_parse_to_a_scheme_and_host() {
        let parsed = parse_url("HTTPS://user:pw@Example.COM:8443/a?q=secret#x").unwrap();
        assert_eq!(parsed.scheme, "https");
        assert_eq!(parsed.host.as_deref(), Some("example.com"));
        assert_eq!(parse_url("tel:+15551234").unwrap().host, None);
        assert_eq!(
            parse_url("weixin://dl/scan").unwrap().host.as_deref(),
            Some("dl")
        );
        assert_eq!(
            parse_url("http://[::1]:80/").unwrap().host.as_deref(),
            Some("::1")
        );
        assert_eq!(parse_url("prefs:root=WIFI").unwrap().scheme, "prefs");
        for bad in [
            "",
            "example.com",
            "://x",
            "1http://x",
            "https://",
            "https:///path",
            "https://a b.com",
            "https://a.com/\n",
        ] {
            assert!(parse_url(bad).is_err(), "{bad:?}");
        }
        assert!(parse_url(&format!("https://a.com/{}", "x".repeat(MAX_URL_LEN))).is_err());
    }

    #[test]
    fn only_web_links_open_unless_the_owner_allows_the_scheme() {
        let open = |url: &str| json!({"type":"open_url","url":url});
        let p = Policy::from_map(|_| None);
        assert!(p.check_url(&open("https://example.com")).is_ok());
        assert!(p.check_url(&open("http://example.com/x")).is_ok());
        for url in [
            "tel:10086",
            "sms:10086",
            "facetime://a@b.c",
            "facetime-audio:1",
            "mailto:a@b.c",
            "itms-services://?action=download-manifest",
            "weixin://dl/scan",
            "prefs:root=WIFI",
            "TEL:1",
        ] {
            let refused = p.check_url(&open(url)).unwrap_err();
            assert_eq!(refused.error(), "url_scheme_not_allowed", "{url}");
            let body = refused.to_json();
            assert_eq!(body["outcome"], "not_sent");
            assert!(body["hint"]
                .as_str()
                .unwrap()
                .contains("IPHONE_USE_ALLOW_URL_SCHEMES"));
        }
        assert!(p.check_url(&json!({"type":"tap","x":1,"y":1})).is_ok());
        assert!(
            !p.is_active(),
            "the scheme policy alone does not switch the other limits on"
        );

        let some = Policy::from_map(|key| {
            (key == "IPHONE_USE_ALLOW_URL_SCHEMES").then(|| "Weixin://, tel:".to_string())
        });
        assert!(some.check_url(&open("weixin://dl/scan")).is_ok());
        assert!(some.check_url(&open("tel:10086")).is_ok());
        assert!(some.check_url(&open("sms:10086")).is_err());
        let any = Policy::from_map(|key| {
            (key == "IPHONE_USE_ALLOW_URL_SCHEMES").then(|| " * ".to_string())
        });
        assert!(any.check_url(&open("sms:10086")).is_ok());
        assert_eq!(describe(&any)["open_url_schemes"], "*");
        assert_eq!(
            describe(&some)["open_url_schemes"],
            json!(["http", "https", "weixin", "tel"])
        );
    }

    #[test]
    fn open_url_leaves_a_blocked_app() {
        let p = policy("com.example.bank", "");
        assert!(!p.needs_foreground(&json!({"type":"open_url","url":"https://example.com"})));
    }

    #[test]
    fn installs_of_blocked_or_unreadable_bundles_are_refused_only_with_a_blocklist() {
        let p = policy("com.example.bank", "");
        let refused = p.check_install(Some("COM.example.bank")).unwrap_err();
        assert_eq!(refused.error(), "blocked_app");
        assert!(p.check_install(Some("com.example.notes")).is_ok());
        let unknown = p.check_install(None).unwrap_err();
        assert_eq!(unknown.to_json()["error"], "install_unverified");
        assert!(Policy::from_map(|_| None).check_install(None).is_ok());
    }

    fn row(kind: &str, label: &str, rect: [f64; 4], depth: u32) -> ElementRow {
        ElementRow {
            kind: kind.to_string(),
            label: label.to_string(),
            rect,
            depth,
            ..Default::default()
        }
    }

    #[test]
    fn point_details_include_all_sensitive_labels_in_hit_control() {
        let rows = vec![
            row("Button", "立即购买", [0., 0., 100., 100.], 0),
            row("StaticText", "确认付款", [10., 10., 80., 80.], 1),
        ];
        assert_eq!(
            point_sensitive_labels(&rows, 50., 50.).unwrap(),
            vec!["立即购买", "确认付款"]
        );
        assert!(check_point(&rows, 50., 50.).unwrap().is_err());
    }

    #[test]
    fn only_paying_words_are_sensitive_not_sending_or_deleting() {
        for label in [
            "立即购买",
            "确认付款 ¥99",
            "转账",
            "充值",
            "开通免密支付",
            "Place Order",
            "Checkout",
        ] {
            assert!(paying_word(label).is_some(), "{label}");
        }
        for label in ["发送", "删除", "分享", "Send", "加入购物车", "Postcode", ""] {
            assert!(paying_word(label).is_none(), "{label}");
        }
        let refused =
            check_sensitive_request(&json!({"type":"tap","label":"立即购买"})).unwrap_err();
        assert_eq!(refused.to_json()["error"], "sensitive_action");
        assert_eq!(refused.to_json()["outcome"], "not_sent");
        assert_eq!(refused.to_json()["retry_safe"], false);
        assert_eq!(refused.to_json()["label"], "立即购买");
        assert!(check_sensitive_request(&json!({"type":"tap","label":"发送"})).is_ok());
        assert!(check_sensitive_request(
            &json!({"type":"tap_locator","locator":{"label_contains":"去结算"}})
        )
        .is_err());
        assert!(check_sensitive_request(&json!({"type":"alert","button":"允许"})).is_ok());
    }

    #[test]
    fn what_each_action_reads_before_it_is_sent() {
        assert_eq!(
            sensitive_read(&json!({"type":"tap","label":"x","kind":"XCUIElementTypeCell"})),
            SensitiveRead::Label {
                label: "x".into(),
                kind: Some("Cell".into())
            }
        );
        assert_eq!(
            sensitive_read(&json!({"type":"tap","x":1.5,"y":0.5})),
            SensitiveRead::None,
            "invalid"
        );
        assert_eq!(
            sensitive_read(&json!({"type":"multi_longpress","points":[{"x":0.3,"y":0.4}]})),
            SensitiveRead::None
        );
        assert_eq!(
            sensitive_read(&json!({"type":"longpress","x":0.5,"y":0.5,"fingers":3})),
            SensitiveRead::None
        );
        assert_eq!(
            sensitive_read(&json!({"type":"text","continue_token":"t"})),
            SensitiveRead::Focused { submits: true }
        );
        assert_eq!(
            sensitive_read(&json!({"type":"tap","element":1,"snapshot":"s"})),
            SensitiveRead::Element
        );
        assert_eq!(
            sensitive_read(&json!({"type":"perform","action":"toggle","element":1})),
            SensitiveRead::Element
        );
        assert_eq!(
            sensitive_read(&json!({"type":"perform","action":"increment","element":1})),
            SensitiveRead::None
        );
        assert_eq!(
            sensitive_read(&json!({"type":"tap","x":0.5,"y":0.25})),
            SensitiveRead::Points(vec![(0.5, 0.25)])
        );
        assert_eq!(
            sensitive_read(&json!({"type":"key","name":"Return"})),
            SensitiveRead::Focused { submits: true }
        );
        assert_eq!(
            sensitive_read(&json!({"type":"key","name":"delete"})),
            SensitiveRead::None
        );
        assert_eq!(
            sensitive_read(&json!({"type":"text","text":"hi\n"})),
            SensitiveRead::Focused { submits: true }
        );
        assert_eq!(
            sensitive_read(&json!({"type":"alert","action":"accept"})),
            SensitiveRead::Alert
        );
        assert_eq!(
            sensitive_read(&json!({"type":"alert","action":"dismiss"})),
            SensitiveRead::None
        );
        assert_eq!(
            sensitive_read(&json!({"type":"swipe","x":0.5,"y":0.5})),
            SensitiveRead::None
        );
    }

    #[test]
    fn a_press_checks_the_control_and_everything_inside_it() {
        let rows = vec![
            row("Cell", "", [0.0, 100.0, 390.0, 60.0], 1),
            row("StaticText", "立即购买", [20.0, 110.0, 100.0, 40.0], 2),
            row("Button", "加入购物车", [0.0, 200.0, 190.0, 60.0], 1),
            row("Other", "", [0.0, 300.0, 390.0, 60.0], 1),
        ];
        assert!(check_pressed_row(&rows, 0).is_err(), "the cell's text pays");
        assert!(check_pressed_row(&rows, 2).is_ok());
        assert!(
            check_point(&rows, 300.0, 130.0).unwrap().is_err(),
            "blank part of the cell"
        );
        assert!(check_point(&rows, 50.0, 230.0).unwrap().is_ok());
        assert!(
            check_point(&rows, 50.0, 330.0).is_none(),
            "no words under the finger"
        );
        assert!(check_label_rows(&rows, "加入购物车", None).is_ok());
        let cell = vec![
            row("Cell", "商品详情", [0.0, 100.0, 390.0, 300.0], 1),
            row("Button", "立即购买", [100.0, 200.0, 190.0, 60.0], 2),
        ];
        assert!(
            check_label_rows(&cell, "商品详情", None).is_err(),
            "the buy button is inside"
        );
        assert!(check_label_rows(&cell, "商品详情", Some("Button")).is_ok());
        // 取消 then a row one level deeper that is drawn elsewhere: the child
        // of an unlabelled sibling container, not 取消's.
        let siblings = vec![
            row("Button", "取消", [0.0, 100.0, 100.0, 44.0], 1),
            row("StaticText", "立即付款", [200.0, 100.0, 100.0, 44.0], 2),
        ];
        assert!(check_pressed_row(&siblings, 0).is_ok());
    }

    #[test]
    fn secret_fields_are_secure_or_named_for_a_password_or_code() {
        assert!(check_typed_row(&row("SecureTextField", "", [0.0; 4], 0)).is_err());
        assert!(check_typed_row(&row("TextField", "短信验证码", [0.0; 4], 0)).is_err());
        assert!(check_typed_row(&row("TextField", "Enter PIN", [0.0; 4], 0)).is_err());
        let mut placeholder = row("TextField", "", [0.0; 4], 0);
        placeholder.placeholder = Some("请输入支付密码".into());
        assert!(check_typed_row(&placeholder).is_err());
        assert!(
            check_typed_row(&row("TextField", "Spinach", [0.0; 4], 0)).is_ok(),
            "whole words"
        );
        assert!(check_typed_row(&row("SearchField", "搜索", [0.0; 4], 0)).is_ok());
        assert!(
            check_typed_row(&row("Button", "密码设置", [0.0; 4], 0)).is_ok(),
            "not a field"
        );
    }

    #[test]
    fn return_is_judged_by_the_keyboard_action_key_and_the_field() {
        let keys = |action: &str| {
            vec![
                row("Key", "q", [0.0, 600.0, 40.0, 50.0], 2),
                row("Key", "p", [350.0, 600.0, 40.0, 50.0], 2),
                row("Key", "space", [0.0, 700.0, 280.0, 50.0], 2),
                row("Button", action, [290.0, 700.0, 100.0, 50.0], 2),
            ]
        };
        assert!(
            check_return(&keys("发送")).is_ok(),
            "sending is the agent's"
        );
        assert!(check_return(&keys("搜索")).is_ok());
        assert!(check_return(&keys("确认付款")).is_err());
        let mut field = row("TextField", "转账金额", [0.0, 100.0, 390.0, 44.0], 1);
        field.focused = Some(true);
        assert!(check_return(&[field]).is_err());
    }

    #[test]
    fn payment_and_bank_apps_by_catalog_name_or_bundle() {
        assert_eq!(
            payment_app(&["com.alipay.iphoneclient".into()]).as_deref(),
            Some("支付宝")
        );
        assert_eq!(
            payment_app(&["com.icbc.iphoneclient".into()]).as_deref(),
            Some("中国工商银行")
        );
        assert!(payment_app(&["com.example.mybank".into()]).is_some());
        assert!(payment_app(&["com.apple.Passbook".into()]).is_some());
        assert!(payment_app(&["com.apple.Preferences".into(), "com.tencent.xin".into()]).is_none());
        assert!(payment_app(&["com.apple.springboard".into()]).is_none());
    }
}
