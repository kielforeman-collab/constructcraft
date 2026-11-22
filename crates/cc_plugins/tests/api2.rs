//! Plugin API 2: buttons, settings, locales, parameter UI hints and the
//! plugin's own data (`ccad.data`).

// Test helpers outside #[test] functions may fail loudly.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use cc_engine::i18n::{Language, language, set_language};
use cc_engine::{CommandKind, ErrorCode, Session};
use cc_plugins::store;
use cc_plugins::{ButtonSize, Limits, Locales, Plugin, PluginHost, ScriptOptions, SettingKind, When, run_script};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

/// A fresh temporary folder with these files.
fn files_in(dir: &Path, files: &[(&str, &str)]) {
    for (path, text) in files {
        let p = dir.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ccad-api2-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A plugin folder `name` with a manifest (header plus `extra`) and a main file.
fn plugin(name: &str, api: u32, extra: &str, main: &str, more: &[(&str, &str)]) -> PathBuf {
    let dir = scratch(name);
    // `extra` may name the plugin itself.
    let name = if extra.trim_start().starts_with("name =") { "" } else { "name = \"Plugin P\"\n" };
    let manifest = format!("id = \"p\"\n{name}version = \"1.0.0\"\napi = {api}\n{extra}");
    files_in(&dir, &[("plugin.toml", &manifest), ("main.lua", main)]);
    files_in(&dir, more);
    dir
}

fn load(dir: &Path) -> Result<Plugin, String> {
    Plugin::load(dir, Limits::default()).map_err(|e| e.message)
}

fn session_with(plugin: Plugin) -> Session {
    let mut host = PluginHost::new();
    host.add(plugin).unwrap();
    let mut s = Session::default();
    s.set_extension(Some(Box::new(host)));
    s
}

const ROOM: &str = r#"
ccad.command { name = "room", title = "%cmd.room%", description = "%cmd.room_help%",
  params = { properties = {
    width = { type = "number", ui = "length", title = "%param.width%" },
    wall = { type = "integer", ui = "element", category = "wall" },
    side = { type = "string", enum = { "north", "south" }, enum_labels = { "%side.n%", "%side.s%" } },
  } },
  run = function() return true end }
ccad.command { name = "plain", run = function() return true end }
"#;

/// Sets the language for a test and puts the previous one back.
struct LanguageGuard(Language);

impl LanguageGuard {
    fn set(lang: Language) -> LanguageGuard {
        let old = language();
        set_language(lang);
        LanguageGuard(old)
    }
}

impl Drop for LanguageGuard {
    fn drop(&mut self) {
        set_language(self.0);
    }
}

#[test]
fn api_version_is_two_and_api_one_plugins_still_load() {
    assert_eq!(cc_plugins::API_VERSION, 3);
    let dir = plugin(
        "api1",
        1,
        "description = \"%desc%\"\n",
        "ccad.command { name = \"c\", title = \"%t%\", params = { properties = { x = { type = \"number\", ui = \"bogus\" } } }, \
         run = function() return { api = ccad.api, h = ccad.settings.height } end }",
        &[("locales/en.toml", "desc = \"Translated\"\nt = \"Title\"\n")],
    );
    let p = load(&dir).unwrap();
    // Under api 1 text stays as written and parameter hints are not checked.
    assert_eq!(p.display_description(), "%desc%");
    assert_eq!(p.command_title("c").as_deref(), Some("%t%"));
    assert!(p.buttons().is_empty() && p.settings().is_empty());
    let mut s = session_with(p);
    assert_eq!(s.execute("p__c", json!({})).unwrap(), json!({"api": 3}));
}

#[test]
fn api_two_sections_are_refused_under_api_one() {
    let dir = plugin("api1-buttons", 1, "[[buttons]]\ncommand = \"plain\"\nlabel = \"P\"\n", ROOM, &[]);
    assert!(load(&dir).unwrap_err().contains("buttons need api = 2"));
    let dir = plugin("api1-settings", 1, "[settings.h]\ntype = \"number\"\ndefault = 1\n", ROOM, &[]);
    assert!(load(&dir).unwrap_err().contains("settings need api = 2"));
    // The same manifests are fine under api 2.
    let dir = plugin(
        "api2-both",
        2,
        "[[buttons]]\ncommand = \"plain\"\nlabel = \"P\"\n[settings.h]\ntype = \"number\"\ndefault = 1\n",
        ROOM,
        &[],
    );
    assert!(load(&dir).is_ok());
}

/// A case's folder name, its button, more files, and a part of the expected error.
type ButtonCase<'a> = (&'a str, &'a str, &'a [(&'a str, &'a str)], &'a str);

const SVG: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 24 24\"/>";

#[test]
fn buttons_are_read_and_checked() {
    let extra = r#"
[[buttons]]
command = "room"
label = "Room"
icon = "icons/room.svg"
panel = "Rooms"
when = "selection:Wall, view:floor_plan"

[[buttons]]
command = "plain"
label = "Plain"
size = "small"
when = "selection:one,selection"
"#;
    let dir = plugin("buttons", 2, extra, ROOM, &[("icons/room.svg", SVG)]);
    let p = load(&dir).unwrap();
    let b = p.buttons();
    assert_eq!(b.len(), 2);
    assert_eq!(b[0].command, "p__room");
    assert_eq!((b[0].label.as_str(), b[0].panel.as_str(), b[0].size), ("Room", "Rooms", ButtonSize::Large));
    assert_eq!(b[0].when, vec![When::SelectionOf("wall".into()), When::View("floor_plan".into())]);
    let icon = b[0].icon.as_ref().unwrap();
    assert!(icon.is_absolute() && icon.ends_with("room.svg") && icon.is_file(), "{icon:?}");
    assert_eq!((b[1].panel.as_str(), b[1].size, b[1].icon.is_none()), ("Plugin P", ButtonSize::Small, true));
    assert_eq!(b[1].when, vec![When::SelectionOne, When::Selection]);
    assert_eq!(When::parse_list("selection:door,view:three_d").unwrap()[0].to_string(), "selection:door");

    let big = format!("<svg>{}</svg>", " ".repeat(64 * 1024));
    let bad: &[ButtonCase] = &[
        ("unknown-cmd", "command = \"nope\"\nlabel = \"X\"", &[], "\"nope\""),
        ("bad-term", "command = \"room\"\nlabel = \"X\"\nwhen = \"hovering\"", &[], "unknown term"),
        ("empty-term", "command = \"room\"\nlabel = \"X\"\nwhen = \"selection,\"", &[], "empty term"),
        ("bad-category", "command = \"room\"\nlabel = \"X\"\nwhen = \"selection:spaceship\"", &[], "unknown category"),
        ("bad-view", "command = \"room\"\nlabel = \"X\"\nwhen = \"view:cinema\"", &[], "unknown view kind"),
        ("outside", "command = \"room\"\nlabel = \"X\"\nicon = \"../room.svg\"", &[], "icon"),
        ("not-svg", "command = \"room\"\nlabel = \"X\"\nicon = \"room.png\"", &[("room.png", "x")], "icon"),
        ("missing-icon", "command = \"room\"\nlabel = \"X\"\nicon = \"none.svg\"", &[], "icon"),
        ("big-icon", "command = \"room\"\nlabel = \"X\"\nicon = \"big.svg\"", &[("big.svg", &big)], "larger than"),
        ("dir-icon", "command = \"room\"\nlabel = \"X\"\nicon = \"d.svg\"", &[("d.svg/x", "")], "not a file"),
        ("long-label", &format!("command = \"room\"\nlabel = \"{}\"", "x".repeat(65)), &[], "label"),
        ("bad-size", "command = \"room\"\nlabel = \"X\"\nsize = \"huge\"", &[], "size"),
        ("unknown-key", "command = \"room\"\nlabel = \"X\"\ncolour = \"red\"", &[], "colour"),
    ];
    for (name, button, more, expected) in bad {
        let dir = plugin(name, 2, &format!("[[buttons]]\n{button}\n"), ROOM, more);
        let e = load(&dir).unwrap_err();
        assert!(e.contains(expected), "{name}: {e}");
    }
    let many: String = (0..65).map(|_| "[[buttons]]\ncommand = \"room\"\nlabel = \"X\"\n").collect();
    assert!(load(&plugin("many-buttons", 2, &many, ROOM, &[])).unwrap_err().contains("at most 64"));
}

const SETTINGS: &str = r#"
[settings.height]
type = "number"
default = 2.8
title = "%settings.height%"
ui = "length"
minimum = 0.5
maximum = 10

[settings.count]
type = "integer"
default = 2
maximum = 5

[settings.mode]
type = "string"
default = "fast"
enum = ["fast", "exact"]
enum_labels = ["%mode.fast%", "Exact"]

[settings.enabled]
type = "boolean"
default = true
description = "Turn it on"
"#;

const SETTINGS_MAIN: &str = r#"
local at_load = ccad.settings.height
local ok, err = pcall(function() ccad.settings.height = 1 end)
assert(not ok and tostring(err):find("read%-only"), "writes must fail")
ccad.command { name = "values", kind = "query", run = function()
  local keys = 0
  for _ in pairs(ccad.settings) do keys = keys + 1 end
  return { at_load = at_load, height = ccad.settings.height, count = ccad.settings.count,
           mode = ccad.settings.mode, enabled = ccad.settings.enabled, keys = keys }
end }
ccad.command { name = "write", run = function() ccad.settings.count = 9 end }
"#;

#[test]
fn settings_have_types_defaults_and_checks() {
    let dir = plugin("settings", 2, SETTINGS, SETTINGS_MAIN, &[]);
    let p = load(&dir).unwrap();
    let settings = p.settings();
    let keys: Vec<&str> = settings.iter().map(|s| s.key.as_str()).collect();
    assert_eq!(keys, ["height", "count", "mode", "enabled"], "in the manifest's order");
    assert_eq!(settings[0].kind, SettingKind::Number);
    assert_eq!(
        (settings[0].ui.as_deref(), settings[0].minimum, settings[0].maximum),
        (Some("length"), Some(0.5), Some(10.0))
    );
    assert_eq!(settings[1].title, "Count", "the key made readable");
    assert_eq!(settings[2].enum_values, ["fast", "exact"]);
    assert_eq!(settings[3].description, "Turn it on");
    assert_eq!(Value::Object(p.setting_values()), json!({"height": 2.8, "count": 2, "mode": "fast", "enabled": true}));
    let mut s = session_with(p);
    let v = s.execute("p__values", json!({})).unwrap();
    assert_eq!(v, json!({"at_load": 2.8, "height": 2.8, "count": 2, "mode": "fast", "enabled": true, "keys": 4}));
    let e = s.execute("p__write", json!({})).unwrap_err();
    assert!(e.message.contains("read-only"), "{}", e.message);

    let bad: &[(&str, &str, &str)] = &[
        ("type", "type = \"date\"\ndefault = 1", "type \"date\""),
        ("no-default", "type = \"number\"", "default"),
        ("default-type", "type = \"integer\"\ndefault = 1.5", "whole number"),
        ("default-bool", "type = \"boolean\"\ndefault = \"yes\"", "true or false"),
        ("default-enum", "type = \"string\"\ndefault = \"c\"\nenum = [\"a\", \"b\"]", "one of a, b"),
        ("default-min", "type = \"number\"\ndefault = 0\nminimum = 1", "at least 1"),
        ("default-max", "type = \"integer\"\ndefault = 7\nmaximum = 5", "at most 5"),
        ("min-max", "type = \"number\"\ndefault = 1\nminimum = 3\nmaximum = 2", "minimum is larger"),
        ("min-string", "type = \"string\"\ndefault = \"a\"\nminimum = 1", "minimum needs"),
        ("enum-number", "type = \"number\"\ndefault = 1\nenum = [\"a\"]", "enum needs"),
        (
            "labels",
            "type = \"string\"\ndefault = \"a\"\nenum = [\"a\", \"b\"]\nenum_labels = [\"A\"]",
            "1 labels for 2",
        ),
        ("labels-alone", "type = \"string\"\ndefault = \"a\"\nenum_labels = [\"A\"]", "enum_labels needs enum"),
        ("ui-string", "type = \"string\"\ndefault = \"a\"\nui = \"length\"", "needs type \"number\""),
        ("ui-other", "type = \"number\"\ndefault = 1\nui = \"angle\"", "ui \"angle\""),
        ("nan", "type = \"number\"\ndefault = 1\nminimum = nan", "finite"),
        ("unknown-key", "type = \"number\"\ndefault = 1\nstep = 2", "step"),
    ];
    for (name, body, expected) in bad {
        let dir = plugin(&format!("setting-{name}"), 2, &format!("[settings.x]\n{body}\n"), "", &[]);
        let e = load(&dir).unwrap_err();
        assert!(e.contains(expected), "{name}: {e}");
    }
    let dir = plugin("setting-key", 2, "[settings.Bad_Key]\ntype = \"number\"\ndefault = 1\n", "", &[]);
    assert!(load(&dir).unwrap_err().contains("settings.Bad_Key"));
}

#[test]
fn settings_values_are_checked_and_changed_live() {
    let dir = plugin("live", 2, SETTINGS, SETTINGS_MAIN, &[]);
    let mut host = PluginHost::new();
    host.add(load(&dir).unwrap()).unwrap();
    assert!(host.set_setting("p", "height", json!(20)).unwrap_err().contains("at most 10"));
    assert!(host.set_setting("p", "mode", json!("slow")).unwrap_err().contains("one of"));
    assert!(host.set_setting("p", "nope", json!(1)).is_err());
    assert!(host.set_setting("q", "height", json!(1)).is_err());
    host.set_setting("p", "height", json!(3.5)).unwrap();
    host.set_setting("p", "count", json!(4.0)).unwrap();
    assert_eq!(host.plugin("p").unwrap().setting_values()["count"], json!(4));
    let mut s = Session::default();
    s.set_extension(Some(Box::new(host)));
    let v = s.execute("p__values", json!({})).unwrap();
    assert_eq!((v["height"].clone(), v["count"].clone(), v["at_load"].clone()), (json!(3.5), json!(4), json!(2.8)));
}

#[test]
fn stored_settings_are_read_with_junk_dropped() {
    let root = scratch("stored");
    let dir = root.join("p");
    let manifest = format!("id = \"p\"\nname = \"P\"\nversion = \"1\"\napi = 2\n{SETTINGS}");
    files_in(&dir, &[("plugin.toml", &manifest), ("main.lua", SETTINGS_MAIN)]);
    // Round trip.
    let mut values = Map::new();
    values.insert("height".into(), json!(4.5));
    values.insert("count".into(), json!(9)); // over the maximum: dropped on load
    values.insert("mode".into(), json!("exact"));
    values.insert("other".into(), json!("unknown key"));
    store::write_settings(&root, "p", &values).unwrap();
    assert_eq!(store::settings_path(&root, "p"), root.join(".settings").join("p.json"));
    assert_eq!(store::read_settings(&root, "p"), values);
    assert!(store::write_settings(&root, "Bad Id", &values).is_err());

    let mut host = PluginHost::new();
    let errors = host.load_folder(&root, Limits::default(), |m| Some(m.requested()));
    assert!(errors.is_empty(), "{errors:?}");
    let p = host.plugin("p").unwrap();
    assert_eq!(Value::Object(p.setting_values()), json!({"height": 4.5, "count": 2, "mode": "exact", "enabled": true}));
    let mut s = Session::default();
    s.set_extension(Some(Box::new(host)));
    assert_eq!(s.execute("p__values", json!({})).unwrap()["at_load"], json!(4.5), "stored values while main.lua runs");

    // A single plugin's folder starts with the defaults.
    let mut single = PluginHost::new();
    assert!(single.load_folder(&dir, Limits::default(), |m| Some(m.requested())).is_empty());
    assert_eq!(single.plugin("p").unwrap().setting_values()["height"], json!(2.8));

    // Junk files give the defaults and never stop loading.
    let file = store::settings_path(&root, "p");
    for junk in ["not json", "[1, 2]", "{\"height\": \"tall\"}"] {
        std::fs::write(&file, junk).unwrap();
        let mut host = PluginHost::new();
        assert!(host.load_folder(&root, Limits::default(), |m| Some(m.requested())).is_empty());
        assert_eq!(host.plugin("p").unwrap().setting_values()["height"], json!(2.8), "{junk}");
    }
    std::fs::write(&file, format!("{{\"height\": 3, \"pad\": \"{}\"}}", "x".repeat(70 * 1024))).unwrap();
    assert!(store::read_settings(&root, "p").is_empty(), "too large");

    // Uninstalling removes the stored settings.
    store::write_settings(&root, "p", &values).unwrap();
    store::uninstall(&root, "p").unwrap();
    assert!(!file.exists() && !dir.exists());
}

const LOCALE_MANIFEST: &str = r#"
name = "%name%"
description = "%description%"

[[buttons]]
command = "room"
label = "%button.room%"
panel = "%panel%"

[settings.height]
type = "number"
default = 2.8
title = "%settings.height%"
description = "%settings.height_help%"

[settings.mode]
type = "string"
default = "a"
enum = ["a", "b"]
enum_labels = ["%mode.a%", "%mode.b%"]
"#;

const EN: &str = r#"
description = "Rooms in a click"
panel = "Rooms"
cmd.room = "Room"
cmd.room_help = "Draws a room"
param.width = "Width"
side.n = "North"
side.s = "South"
[button]
room = "New room"
[settings]
height = "Height"
height_help = "Wall height"
[mode]
a = "Mode A"
"#;

const PT_BR: &str = r#"
name = "Salas rápidas"
cmd.room = "Sala"
[settings]
height = "Altura"
[mode]
a = "Modo A"
"#;

const ZH_CN: &str = "cmd.room = \"房间\"\n";

#[test]
fn locales_resolve_by_language_when_listed() {
    let dir = plugin(
        "locales",
        2,
        LOCALE_MANIFEST,
        ROOM,
        &[
            ("locales/en.toml", EN),
            ("locales/pt-BR.toml", PT_BR),
            ("locales/zh-CN.toml", ZH_CN),
            ("locales/xx.toml", "this is not toml at all ["),
            ("locales/readme.txt", "ignored"),
        ],
    );
    let locales = Locales::load(&dir).unwrap();
    assert_eq!(locales.codes().collect::<Vec<_>>(), ["en", "pt-BR", "zh-CN"]);
    // Exact code, then the base language, then English, then the key.
    assert_eq!(locales.resolve_for("%cmd.room%", "pt-BR"), "Sala");
    assert_eq!(locales.resolve_for("%cmd.room%", "zh-TW"), "房间");
    assert_eq!(locales.resolve_for("%cmd.room%", "de"), "Room");
    assert_eq!(locales.resolve_for("%cmd.room_help%", "pt-BR"), "Draws a room");
    assert_eq!(locales.resolve_for("%missing.key%", "pt-BR"), "missing.key");
    assert_eq!(locales.resolve_for("50% off", "en"), "50% off");
    assert_eq!(locales.resolve_for("%a b%", "en"), "%a b%");

    let host = {
        let mut host = PluginHost::new();
        host.add(load(&dir).unwrap()).unwrap();
        host
    };
    let p = host.plugin("p").unwrap();
    {
        let _guard = LanguageGuard::set(Language::En);
        assert_eq!(p.display_name(), "name", "a key found nowhere shows as itself");
        assert_eq!(p.display_description(), "Rooms in a click");
        let b = &p.buttons()[0];
        assert_eq!((b.label.as_str(), b.panel.as_str()), ("New room", "Rooms"));
        let settings = p.settings();
        assert_eq!((settings[0].title.as_str(), settings[0].description.as_str()), ("Height", "Wall height"));
        assert_eq!(settings[1].enum_labels, ["Mode A", "mode.b"]);
        assert_eq!(host.command_title("p__room").as_deref(), Some("Room"));
        assert_eq!(host.command_title("p__plain"), None);
        assert_eq!(host.command_title("p__nope"), None);
        let info = cc_engine::Extension::commands(&host).into_iter().find(|c| c.name == "p__room").unwrap();
        assert_eq!(info.description, "Draws a room");
        assert_eq!(info.input_schema["properties"]["width"]["title"], "Width");
        assert_eq!(info.input_schema["properties"]["side"]["enum_labels"], json!(["North", "South"]));
    }
    {
        // The language changes without reloading the plugin.
        let _guard = LanguageGuard::set(Language::PtBr);
        assert_eq!(p.display_name(), "Salas rápidas");
        assert_eq!(p.settings()[0].title, "Altura");
        assert_eq!(p.settings()[1].enum_labels, ["Modo A", "mode.b"]);
        let info = cc_engine::Extension::commands(&host).into_iter().find(|c| c.name == "p__room").unwrap();
        assert_eq!(info.description, "Draws a room", "falls back to English");
        assert_eq!(host.command_title("p__room").as_deref(), Some("Sala"));
        let summary = host.summary();
        assert_eq!(summary[0]["name"], "Salas rápidas");
        assert_eq!(summary[0]["commands"][0]["title"], "Sala");
        assert_eq!(summary[0]["buttons"][0]["command"], "p__room");
        assert_eq!(summary[0]["settings"][0]["title"], "Altura");
        assert_eq!(summary[0]["settings"][0]["value"], 2.8);
    }
    let _guard = LanguageGuard::set(Language::ZhCn);
    assert_eq!(host.command_title("p__room").as_deref(), Some("房间"));
}

#[test]
fn bad_locale_files_are_refused() {
    let cases: &[(&str, &str, &str)] = &[
        ("syntax", "x = ", "locales/en.toml"),
        ("number", "x = 1", "x: not a string"),
        ("array", "[a]\nb = [\"c\"]", "a.b: not a string"),
    ];
    for (name, text, expected) in cases {
        let dir = plugin(&format!("locale-{name}"), 2, "", "", &[("locales/en.toml", text)]);
        let e = load(&dir).unwrap_err();
        assert!(e.contains(expected), "{name}: {e}");
    }
    let big = format!("x = \"{}\"", "y".repeat(257 * 1024));
    let dir = plugin("locale-big", 2, "", "", &[("locales/en.toml", &big)]);
    assert!(load(&dir).unwrap_err().contains("larger than"));
}

#[test]
fn parameter_ui_hints_are_checked() {
    let ok = r#"
ccad.command { name = "c", params = { properties = {
  a = { type = "number", ui = "length" },
  b = { type = "integer", ui = "element", category = "door" },
  c = { type = "array", ui = "elements", category = "wall", items = { type = "integer" } },
  d = { type = "array", ui = "point", items = { type = "number" } },
  e = { type = "integer", ui = "level" },
  f = { type = "integer", ui = "view" },
  g = { type = "integer", ui = "type", category = "wall" },
  h = { type = "string", ui = "material", title = "Material" },
  i = { type = "string", enum = { "x", "y" }, enum_labels = { "X", "Y" } },
} }, run = function() return true end }
"#;
    let p = load(&plugin("hints-ok", 2, "", ok, &[])).unwrap();
    let info = p.commands().next().unwrap();
    assert_eq!(info.input_schema["properties"]["b"]["category"], "door", "hints pass through");
    assert_eq!(info.kind, CommandKind::Edit);

    let bad: &[(&str, &str, &str)] = &[
        ("unknown-ui", "{ type = \"number\", ui = \"angle\" }", "ui \"angle\""),
        ("length", "{ type = \"integer\", ui = \"length\" }", "needs type \"number\""),
        ("element", "{ type = \"number\", ui = \"element\" }", "needs type \"integer\""),
        ("elements", "{ type = \"integer\", ui = \"elements\" }", "needs type \"array\""),
        ("point", "{ type = \"object\", ui = \"point\" }", "needs type \"array\""),
        ("level", "{ type = \"string\", ui = \"level\" }", "needs type \"integer\""),
        ("view", "{ ui = \"view\" }", "needs type \"integer\""),
        ("type", "{ type = \"string\", ui = \"type\" }", "needs type \"integer\""),
        ("material", "{ type = \"integer\", ui = \"material\" }", "needs type \"string\""),
        ("ui-not-string", "{ type = \"number\", ui = 3 }", "ui must be a string"),
        ("category-alone", "{ type = \"integer\", category = \"wall\" }", "category needs"),
        ("category-level", "{ type = \"integer\", ui = \"level\", category = \"wall\" }", "category needs"),
        ("category-unknown", "{ type = \"integer\", ui = \"element\", category = \"ufo\" }", "unknown category"),
        ("labels-alone", "{ type = \"string\", enum_labels = { \"A\" } }", "enum_labels needs enum"),
        ("labels-count", "{ type = \"string\", enum = { \"a\", \"b\" }, enum_labels = { \"A\" } }", "2 strings"),
        ("title", "{ type = \"string\", title = 3 }", "title must be a string"),
    ];
    for (name, prop, expected) in bad {
        let main = format!(
            "ccad.command {{ name = \"c\", params = {{ properties = {{ x = {prop} }} }}, run = function() end }}"
        );
        let e = load(&plugin(&format!("hints-{name}"), 2, "", &main, &[])).unwrap_err();
        assert!(e.contains("params.x") && e.contains(expected), "{name}: {e}");
    }
}

const DATA_MAIN: &str = r#"
ccad.command { name = "tag", params = { properties = { element = { type = "integer", ui = "element" } } },
  run = function(a)
    ccad.data.set(a.element, { note = "hello" })
    ccad.data.set(nil, { count = 1 })
    return { element = ccad.data.get(a.element), project = ccad.data.get() }
  end }
ccad.command { name = "read", kind = "query", run = function()
  local p = ccad.data.get()
  return { list = ccad.data.list(), project = p == ccad.null and "none" or p }
end }
ccad.command { name = "forget", run = function() ccad.data.set(nil, nil) end }
ccad.command { name = "call", run = function(a) return ccad.run(a.name, a.args) end }
ccad.command { name = "try", run = function(a)
  local r, err = ccad.try(a.name, a.args)
  return { code = err and err.code or "ok", message = err and err.message or "" }
end }
ccad.command { name = "sneaky", kind = "query", run = function() return ccad.data.set(nil, 1) end }
"#;

#[test]
fn plugins_keep_their_own_data() {
    let p = load(&plugin("data", 2, "", DATA_MAIN, &[])).unwrap();
    let mut s = session_with(p);
    let wall = s.execute("wall_create", json!({"start": [0, 0], "end": [4, 0]})).unwrap()["id"].as_u64().unwrap();
    let before = s.doc.undo_depth();
    let out = s.execute("p__tag", json!({"element": wall})).unwrap();
    assert_eq!(out, json!({"element": {"note": "hello"}, "project": {"count": 1}}));
    assert_eq!(s.doc.undo_depth(), before + 1, "one undo step with the command");
    let read = s.execute("p__read", json!({})).unwrap();
    assert_eq!(read, json!({"list": {"project": true, "elements": [wall]}, "project": {"count": 1}}));
    // The engine sees it under the plugin's id.
    assert_eq!(
        s.execute("plugin_data_get", json!({"plugin": "p", "element": wall})).unwrap(),
        json!({"note": "hello"})
    );
    s.execute("undo", json!({})).unwrap();
    let read = s.execute("p__read", json!({})).unwrap();
    assert_eq!(read, json!({"list": {"project": false, "elements": []}, "project": "none"}));
    s.execute("redo", json!({})).unwrap();
    s.execute("p__forget", json!({})).unwrap();
    assert_eq!(s.execute("p__read", json!({})).unwrap()["list"]["project"], false);

    // Another plugin's data is out of reach, through ccad.run and ccad.try.
    for name in ["plugin_data_set", "plugin_data_get", "plugin_data_list"] {
        let args = json!({"plugin": "other", "data": 1});
        let e = s.execute("p__call", json!({"name": name, "args": args})).unwrap_err();
        assert_eq!(e.code, ErrorCode::Rejected, "{name}");
        assert!(e.message.contains("only its own data"), "{}", e.message);
        let caught = s.execute("p__try", json!({"name": name, "args": {"data": 1}})).unwrap();
        assert_eq!(caught["code"], "rejected", "{name} without a plugin");
    }
    // Its own id through ccad.run works.
    s.execute("p__call", json!({"name": "plugin_data_set", "args": {"plugin": "p", "data": 5}})).unwrap();
    assert_eq!(s.execute("plugin_data_get", json!({"plugin": "p"})).unwrap(), json!(5));
    // A query command cannot write.
    let e = s.execute("p__sneaky", json!({})).unwrap_err();
    assert_eq!(e.code, ErrorCode::Rejected);
    // The user's scripts reach any plugin's data.
    let out = run_script(
        &mut s,
        "ccad.run('plugin_data_set', { plugin = 'other', data = 2 }); return ccad.run('plugin_data_get', { plugin = 'other' })",
        "t.lua",
        json!({}),
        ScriptOptions::default(),
    );
    assert_eq!(out.result.unwrap(), json!(2));
    // ccad.data exists only in plugins.
    let out = run_script(&mut s, "return ccad.data == nil", "t.lua", json!({}), ScriptOptions::default());
    assert_eq!(out.result.unwrap(), json!(true));
}
