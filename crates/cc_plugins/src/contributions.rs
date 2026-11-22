//! What a plugin declares in its `plugin.toml` besides commands (plugin API
//! 2): ribbon buttons with their availability conditions ([`When`]), settings
//! ([`Setting`]), exporters and importers ([`FileAction`]), and the UI hints
//! of command parameters.
//!
//! The manifest's own text may be a `%key%` looked up in the plugin's
//! locales (see [`crate::locales`]) when it is shown; the types here keep the
//! text as written, and [`crate::Plugin`] resolves it.

use cc_core::Category;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::path::PathBuf;

/// Most buttons and settings of one plugin.
pub const MAX_BUTTONS: usize = 64;
pub const MAX_SETTINGS: usize = 64;
/// Longest button label and panel name, in characters (as written).
pub const MAX_LABEL: usize = 64;
/// Largest button icon, in bytes.
pub const MAX_ICON: u64 = 64 * 1024;
/// Longest setting title and description, in characters (as written).
const MAX_TITLE: usize = 100;
const MAX_DESCRIPTION: usize = 1000;
/// Longest string value of a setting, in bytes, and most `enum` options.
const MAX_STRING_VALUE: usize = 4096;
const MAX_OPTIONS: usize = 256;

/// The view kinds a `view:<kind>` condition names.
pub const VIEW_KINDS: [&str; 10] = [
    "floor_plan",
    "ceiling_plan",
    "three_d",
    "elevation",
    "section",
    "legend",
    "drafting",
    "schedule",
    "diagram",
    "sheet",
];

/// Most exporters, and most importers, of one plugin.
pub const MAX_FILE_ACTIONS: usize = 16;
/// Most `extensions` of a path parameter, and longest one.
pub const MAX_EXTENSIONS: usize = 16;
const MAX_EXTENSION: usize = 16;

/// The `ui` hints of command parameters and the JSON type each needs.
pub const PARAM_UI: [(&str, &str); 10] = [
    ("length", "number"),
    ("element", "integer"),
    ("elements", "array"),
    ("point", "array"),
    ("level", "integer"),
    ("view", "integer"),
    ("type", "integer"),
    ("material", "string"),
    ("save_path", "string"),
    ("open_path", "string"),
];

/// The `ui` hints of a file the command writes (`save_path`) or reads
/// (`open_path`): granted to the command when it runs.
pub const PATH_UI: [&str; 2] = ["save_path", "open_path"];

/// The `ui` hints that may carry a `category`.
const UI_WITH_CATEGORY: [&str; 3] = ["element", "elements", "type"];

/// A `[[buttons]]` entry of the manifest, as written.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ButtonSpec {
    /// A command the plugin registers (its short name).
    pub command: String,
    pub label: String,
    /// An `.svg` file inside the plugin's folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// The ribbon panel (default: the plugin's name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panel: Option<String>,
    /// `large` (default) or `small`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,
    /// When the button is available: comma-separated [`When`] terms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
}

/// A button's size on the ribbon.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ButtonSize {
    #[default]
    Large,
    Small,
}

/// One term of a button's `when`; a button is available when all hold.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum When {
    /// `selection`: at least one element selected.
    Selection,
    /// `selection:one`: exactly one element selected.
    SelectionOne,
    /// `selection:<category>`: at least one element selected, all of this
    /// category (its engine name, lowercase: `wall`, `door`…).
    SelectionOf(String),
    /// `view:<kind>`: the active view is of this kind (one of [`VIEW_KINDS`]).
    View(String),
}

impl When {
    /// Read a `when` text: comma-separated terms, all of which must hold.
    pub fn parse_list(text: &str) -> Result<Vec<When>, String> {
        text.split(',').map(|t| When::parse(t.trim())).collect()
    }

    /// Read one term.
    pub fn parse(term: &str) -> Result<When, String> {
        if term == "selection" {
            return Ok(When::Selection);
        }
        if let Some(rest) = term.strip_prefix("selection:") {
            if rest == "one" {
                return Ok(When::SelectionOne);
            }
            return match category_name(rest) {
                Some(name) => Ok(When::SelectionOf(name)),
                None => Err(format!("when \"{term}\": unknown category \"{rest}\" (see categories_list)")),
            };
        }
        if let Some(kind) = term.strip_prefix("view:") {
            if VIEW_KINDS.contains(&kind) {
                return Ok(When::View(kind.to_string()));
            }
            return Err(format!("when \"{term}\": unknown view kind \"{kind}\" ({})", VIEW_KINDS.join(", ")));
        }
        if term.is_empty() {
            return Err("when: an empty term".into());
        }
        Err(format!("when \"{term}\": unknown term (selection, selection:one, selection:<category> or view:<kind>)"))
    }
}

impl std::fmt::Display for When {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            When::Selection => write!(f, "selection"),
            When::SelectionOne => write!(f, "selection:one"),
            When::SelectionOf(c) => write!(f, "selection:{c}"),
            When::View(k) => write!(f, "view:{k}"),
        }
    }
}

/// An element category's engine name (as `categories_list` and
/// `elements_list category=` write it), from a name in any case.
pub fn category_name(name: &str) -> Option<String> {
    Category::NAMED
        .iter()
        .map(|c| format!("{c:?}"))
        .find(|c| c.eq_ignore_ascii_case(name.trim()))
        .map(|c| c.to_lowercase())
}

/// A plugin's button, ready to show (text in the current language).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Button {
    /// The command it runs, full name (`<id>__<command>`).
    pub command: String,
    pub label: String,
    pub panel: String,
    /// The icon's absolute path (inside the plugin's folder), if any.
    pub icon: Option<PathBuf>,
    pub size: ButtonSize,
    /// All must hold for the button to be available (empty: always).
    #[serde(serialize_with = "when_texts")]
    pub when: Vec<When>,
}

fn when_texts<S: serde::Serializer>(when: &[When], s: S) -> Result<S::Ok, S::Error> {
    s.collect_seq(when.iter().map(ToString::to_string))
}

/// Characters of `s`, for the limits on text written by the plugin author.
fn chars(s: &str) -> usize {
    s.chars().count()
}

impl ButtonSpec {
    /// Check what does not need the plugin's folder; returns the size and
    /// the parsed `when`.
    pub(crate) fn check(&self) -> Result<(ButtonSize, Vec<When>), String> {
        if !crate::plugin::valid_command_name(&self.command) {
            return Err(format!("command \"{}\": a command name the plugin registers", self.command));
        }
        if self.label.trim().is_empty() || chars(&self.label) > MAX_LABEL {
            return Err(format!("label: 1 to {MAX_LABEL} characters"));
        }
        if let Some(panel) = &self.panel
            && (panel.trim().is_empty() || chars(panel) > MAX_LABEL)
        {
            return Err(format!("panel: 1 to {MAX_LABEL} characters"));
        }
        let size = match self.size.as_deref() {
            None | Some("large") => ButtonSize::Large,
            Some("small") => ButtonSize::Small,
            Some(other) => return Err(format!("size \"{other}\" (large or small)")),
        };
        if let Some(icon) = &self.icon
            && crate::plugin::plain_path(icon, "svg").is_none()
        {
            return Err(format!("icon \"{icon}\": an .svg file inside the plugin's folder"));
        }
        let when = match &self.when {
            Some(text) => When::parse_list(text)?,
            None => Vec::new(),
        };
        Ok((size, when))
    }
}

/// A `[settings.<key>]` entry of the manifest, as written.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SettingSpec {
    /// `boolean`, `number`, `integer` or `string`.
    #[serde(rename = "type")]
    pub kind: String,
    pub default: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// `length` (numbers only): shown and typed in the project's units.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui: Option<String>,
    /// The allowed values (strings only).
    #[serde(default, rename = "enum", skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
    /// What each of `enum` is called, same length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enum_labels: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum: Option<f64>,
}

/// The type of a setting's value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SettingKind {
    Boolean,
    Number,
    Integer,
    String,
}

/// A plugin's setting: its schema (text in the current language when it
/// comes from [`crate::Plugin::settings`]).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Setting {
    pub key: String,
    #[serde(rename = "type")]
    pub kind: SettingKind,
    pub default: Value,
    /// The manifest's `title`, or the key made readable (`default_height`
    /// → `Default height`).
    pub title: String,
    pub description: String,
    /// `length` for a number shown in the project's units.
    pub ui: Option<String>,
    /// The allowed values of a string setting (empty: any).
    #[serde(rename = "enum")]
    pub enum_values: Vec<String>,
    /// What each allowed value is called (the values themselves when the
    /// manifest gives no labels).
    pub enum_labels: Vec<String>,
    pub minimum: Option<f64>,
    pub maximum: Option<f64>,
}

/// `default_height` → `Default height`.
fn humanize(key: &str) -> String {
    let spaced = key.replace('_', " ");
    let mut it = spaced.chars();
    match it.next() {
        Some(first) => first.to_uppercase().chain(it).collect(),
        None => String::new(),
    }
}

impl SettingSpec {
    /// Check the entry `key` and make its [`Setting`] (text as written).
    pub(crate) fn setting(&self, key: &str) -> Result<Setting, String> {
        let err = |m: String| format!("settings.{key}: {m}");
        if !cc_core::plugin_data::valid_plugin_name(key) {
            return Err(
                err("a key of lowercase letters, digits and single underscores, starting with a letter".into()),
            );
        }
        let kind = match self.kind.as_str() {
            "boolean" => SettingKind::Boolean,
            "number" => SettingKind::Number,
            "integer" => SettingKind::Integer,
            "string" => SettingKind::String,
            other => return Err(err(format!("type \"{other}\" (boolean, number, integer or string)"))),
        };
        if let Some(t) = &self.title
            && (t.trim().is_empty() || chars(t) > MAX_TITLE)
        {
            return Err(err(format!("title: 1 to {MAX_TITLE} characters")));
        }
        if self.description.as_deref().is_some_and(|d| chars(d) > MAX_DESCRIPTION) {
            return Err(err(format!("description: at most {MAX_DESCRIPTION} characters")));
        }
        match self.ui.as_deref() {
            None => {}
            Some("length") if kind == SettingKind::Number => {}
            Some("length") => return Err(err("ui \"length\" needs type \"number\"".into())),
            Some(other) => return Err(err(format!("ui \"{other}\" (only \"length\", for numbers)"))),
        }
        let options = self.options.clone().unwrap_or_default();
        if self.options.is_some() {
            if kind != SettingKind::String {
                return Err(err("enum needs type \"string\"".into()));
            }
            if options.is_empty() || options.len() > MAX_OPTIONS {
                return Err(err(format!("enum: 1 to {MAX_OPTIONS} values")));
            }
            if options.iter().enumerate().any(|(i, o)| options[..i].contains(o)) {
                return Err(err("enum: a value is listed twice".into()));
            }
        }
        let labels = match &self.enum_labels {
            Some(_) if self.options.is_none() => return Err(err("enum_labels needs enum".into())),
            Some(l) if l.len() != options.len() => {
                return Err(err(format!("enum_labels: {} labels for {} values", l.len(), options.len())));
            }
            Some(l) if l.iter().any(|l| l.trim().is_empty() || chars(l) > MAX_TITLE) => {
                return Err(err(format!("enum_labels: each 1 to {MAX_TITLE} characters")));
            }
            Some(l) => l.clone(),
            None => options.clone(),
        };
        for (name, bound) in [("minimum", self.minimum), ("maximum", self.maximum)] {
            if let Some(b) = bound {
                if !matches!(kind, SettingKind::Number | SettingKind::Integer) {
                    return Err(err(format!("{name} needs type \"number\" or \"integer\"")));
                }
                if !b.is_finite() {
                    return Err(err(format!("{name}: a finite number")));
                }
            }
        }
        if let (Some(min), Some(max)) = (self.minimum, self.maximum)
            && min > max
        {
            return Err(err("minimum is larger than maximum".into()));
        }
        let mut setting = Setting {
            key: key.to_string(),
            kind,
            default: Value::Null,
            title: self.title.clone().unwrap_or_else(|| humanize(key)),
            description: self.description.clone().unwrap_or_default(),
            ui: self.ui.clone(),
            enum_values: options,
            enum_labels: labels,
            minimum: self.minimum,
            maximum: self.maximum,
        };
        setting.default = setting.validate(&self.default).map_err(|e| err(format!("default: {e}")))?;
        Ok(setting)
    }
}

impl Setting {
    /// `value` if it fits this setting (type, `enum`, `minimum`, `maximum`),
    /// as stored (a whole number for an integer, a number for a number).
    pub fn validate(&self, value: &Value) -> Result<Value, String> {
        let value = match self.kind {
            SettingKind::Boolean => match value {
                Value::Bool(_) => value.clone(),
                _ => return Err(format!("{}: true or false", self.key)),
            },
            SettingKind::String => match value {
                Value::String(s) if s.len() > MAX_STRING_VALUE => {
                    return Err(format!("{}: at most {MAX_STRING_VALUE} bytes", self.key));
                }
                Value::String(s) if !self.enum_values.is_empty() && !self.enum_values.contains(s) => {
                    return Err(format!("{}: one of {}", self.key, self.enum_values.join(", ")));
                }
                Value::String(_) => value.clone(),
                _ => return Err(format!("{}: a string", self.key)),
            },
            SettingKind::Number => match value.as_f64() {
                Some(x) if x.is_finite() => value.clone(),
                _ => return Err(format!("{}: a number", self.key)),
            },
            SettingKind::Integer => match (value.as_i64(), value.as_f64()) {
                (Some(i), _) => json!(i),
                // A whole number written as a float (2.0).
                (None, Some(x)) if x.fract() == 0.0 && x.abs() < 9.0e15 => json!(x as i64),
                _ => return Err(format!("{}: a whole number", self.key)),
            },
        };
        if let Some(x) = value.as_f64() {
            if self.minimum.is_some_and(|min| x < min) {
                return Err(format!("{}: at least {}", self.key, self.minimum.unwrap_or_default()));
            }
            if self.maximum.is_some_and(|max| x > max) {
                return Err(format!("{}: at most {}", self.key, self.maximum.unwrap_or_default()));
            }
        }
        Ok(value)
    }

    /// The same setting with its text passed through `resolve`.
    pub(crate) fn resolved(&self, resolve: impl Fn(&str) -> String) -> Setting {
        Setting {
            title: resolve(&self.title),
            description: resolve(&self.description),
            enum_labels: self.enum_labels.iter().map(|l| resolve(l)).collect(),
            ..self.clone()
        }
    }
}

/// The value of each setting: the stored one when it fits, else the default
/// (unknown keys and values that do not fit are dropped).
pub fn current_values(settings: &[Setting], stored: &Map<String, Value>) -> Map<String, Value> {
    settings
        .iter()
        .map(|s| {
            let value = stored.get(&s.key).and_then(|v| s.validate(v).ok()).unwrap_or_else(|| s.default.clone());
            (s.key.clone(), value)
        })
        .collect()
}

/// Settings with their values, as JSON (`ccad plugins`, `ccad plugins
/// settings`): each setting's fields and its `value`.
pub fn settings_json(settings: &[Setting], values: &Map<String, Value>) -> Value {
    Value::Array(
        settings
            .iter()
            .map(|s| {
                let mut v = serde_json::to_value(s).unwrap_or_default();
                if let Some(obj) = v.as_object_mut() {
                    obj.insert("value".into(), values.get(&s.key).cloned().unwrap_or_else(|| s.default.clone()));
                }
                v
            })
            .collect(),
    )
}

/// Check the UI hints of a command's parameters (`params.properties.*`):
/// `ui` (one of [`PARAM_UI`], with its JSON type), `category` (with
/// `element`, `elements` or `type`), `enum_labels` (as many strings as
/// `enum`) and `title` (a string).
pub(crate) fn check_param_hints(schema: &Value) -> Result<(), String> {
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else { return Ok(()) };
    for (name, prop) in properties {
        let err = |m: String| format!("params.{name}: {m}");
        let Some(prop) = prop.as_object() else { continue };
        let ty = prop.get("type").and_then(Value::as_str);
        let ui = match prop.get("ui") {
            None => None,
            Some(Value::String(ui)) => {
                let Some((_, needs)) = PARAM_UI.iter().find(|(u, _)| u == ui) else {
                    let known: Vec<&str> = PARAM_UI.iter().map(|(u, _)| *u).collect();
                    return Err(err(format!("ui \"{ui}\" ({})", known.join(", "))));
                };
                if ty != Some(*needs) {
                    return Err(err(format!("ui \"{ui}\" needs type \"{needs}\"")));
                }
                Some(ui.as_str())
            }
            Some(_) => return Err(err("ui must be a string".into())),
        };
        match prop.get("category") {
            None => {}
            Some(Value::String(c)) if ui.is_some_and(|u| UI_WITH_CATEGORY.contains(&u)) => {
                if category_name(c).is_none() {
                    return Err(err(format!("unknown category \"{c}\" (see categories_list)")));
                }
            }
            Some(Value::String(_)) => {
                return Err(err("category needs ui \"element\", \"elements\" or \"type\"".into()));
            }
            Some(_) => return Err(err("category must be a string".into())),
        }
        if let Some(labels) = prop.get("enum_labels") {
            let Some(options) = prop.get("enum").and_then(Value::as_array) else {
                return Err(err("enum_labels needs enum".into()));
            };
            match labels.as_array() {
                Some(l) if l.len() == options.len() && l.iter().all(Value::is_string) => {}
                _ => return Err(err(format!("enum_labels: {} strings, one per enum value", options.len()))),
            }
        }
        if prop.get("title").is_some_and(|t| !t.is_string()) {
            return Err(err("title must be a string".into()));
        }
        match prop.get("extensions") {
            None => {}
            Some(_) if !ui.is_some_and(|u| PATH_UI.contains(&u)) => {
                return Err(err("extensions needs ui \"save_path\" or \"open_path\"".into()));
            }
            Some(list) => {
                check_extensions(list).map_err(err)?;
            }
        }
    }
    Ok(())
}

/// A path parameter's `extensions`: 1 to [`MAX_EXTENSIONS`] lowercase
/// letters and digits each (1 to 16), without the dot, none twice.
fn check_extensions(list: &Value) -> Result<Vec<String>, String> {
    let bad = || {
        format!(
            "extensions: 1 to {MAX_EXTENSIONS} different extensions, each 1 to {MAX_EXTENSION} lowercase letters              or digits, without the dot"
        )
    };
    let items = list.as_array().ok_or_else(bad)?;
    if items.is_empty() || items.len() > MAX_EXTENSIONS {
        return Err(bad());
    }
    let mut out: Vec<String> = Vec::new();
    for item in items {
        let ext = item.as_str().ok_or_else(bad)?;
        let plain = !ext.is_empty()
            && ext.len() <= MAX_EXTENSION
            && ext.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
        if !plain || out.iter().any(|o| o == ext) {
            return Err(bad());
        }
        out.push(ext.to_string());
    }
    Ok(out)
}

/// The path parameters of a command's schema: `(name, ui, extensions)` of
/// each property with `ui = "save_path"` or `"open_path"`, in name order.
pub fn path_params(schema: &Value) -> Vec<(String, String, Vec<String>)> {
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else { return Vec::new() };
    properties
        .iter()
        .filter_map(|(name, prop)| {
            let ui = prop.get("ui").and_then(Value::as_str).filter(|u| PATH_UI.contains(u))?;
            let extensions = prop.get("extensions").and_then(|l| check_extensions(l).ok()).unwrap_or_default();
            Some((name.clone(), ui.to_string(), extensions))
        })
        .collect()
}

/// An `[[exporters]]` or `[[importers]]` entry of the manifest, as written.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileActionSpec {
    /// A command the plugin registers (its short name), with exactly one
    /// `save_path` parameter (an exporter) or `open_path` one (an importer).
    pub command: String,
    /// What the app's Export or Import entry says; may be `%key%`.
    pub label: String,
}

impl FileActionSpec {
    /// Check what does not need the plugin's commands.
    pub(crate) fn check(&self) -> Result<(), String> {
        if !crate::plugin::valid_command_name(&self.command) {
            return Err(format!("command \"{}\": a command name the plugin registers", self.command));
        }
        if self.label.trim().is_empty() || chars(&self.label) > MAX_LABEL {
            return Err(format!("label: 1 to {MAX_LABEL} characters"));
        }
        Ok(())
    }
}

/// A plugin's exporter or importer, ready to show (label in the current
/// language).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileAction {
    /// The command it runs, full name (`<id>__<command>`).
    pub command: String,
    pub label: String,
    /// The command's parameter that takes the path.
    pub param: String,
    /// The extensions the file may have (empty: any), lowercase without the dot.
    pub extensions: Vec<String>,
}

/// Most component catalog sources one plugin may declare (api 3).
pub const MAX_CATALOGS: usize = 8;

/// A `[[catalogs]]` entry of the manifest, as written (api 3).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogSpec {
    /// Short identifier unique within the plugin (same rule as a plugin id).
    pub id: String,
    /// Tab label in the Component Library window; may be `%key%`.
    pub label: String,
    /// Optional description or attribution shown in the sidebar; may be `%key%`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

impl CatalogSpec {
    pub(crate) fn check(&self) -> Result<(), String> {
        if !crate::plugin::valid_id(&self.id) {
            return Err(format!("id \"{}\": lowercase letters, digits and single underscores", self.id));
        }
        if self.label.trim().is_empty() || chars(&self.label) > MAX_LABEL {
            return Err(format!("label: 1 to {MAX_LABEL} characters"));
        }
        if chars(&self.description) > MAX_DESCRIPTION {
            return Err(format!("description: at most {MAX_DESCRIPTION} characters"));
        }
        Ok(())
    }
}

/// A plugin's catalog source, ready to show in the Component Library (text
/// resolved in the current language).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Catalog {
    pub plugin_id: String,
    pub plugin_name: String,
    pub id: String,
    pub label: String,
    pub description: String,
}

/// The schema with each property's `title`, `description` and
/// `enum_labels` passed through `resolve`.
pub(crate) fn resolve_schema(schema: &Value, resolve: impl Fn(&str) -> String) -> Value {
    let mut schema = schema.clone();
    if let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) {
        for prop in properties.values_mut().filter_map(Value::as_object_mut) {
            for key in ["title", "description"] {
                if let Some(Value::String(s)) = prop.get_mut(key) {
                    *s = resolve(s);
                }
            }
            if let Some(Value::Array(labels)) = prop.get_mut("enum_labels") {
                for l in labels.iter_mut() {
                    if let Value::String(s) = l {
                        *s = resolve(s);
                    }
                }
            }
        }
    }
    schema
}
