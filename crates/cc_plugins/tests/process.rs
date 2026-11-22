//! External programs: the `tools` permission, `[[tools]]`, the paths the
//! user chooses (`.tools/<id>.json`), `ccad.process.run` (no shell, the run
//! folder, inputs and outputs, limits, timeouts, exit codes), and plugin
//! commands that wait for a program without holding the session.

// Test helpers outside #[test] functions may fail loudly.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use cc_engine::{ErrorCode, Session};
use cc_plugins::{Limits, Permissions, Plugin, PluginHost};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The program the tests run (see `src/bin/test_tool.rs`).
const TOOL: &str = env!("CARGO_BIN_EXE_cc_plugins_test_tool");

const TOOLS: &str = r#"
[[tools]]
id = "helper"
name = "%tool.helper%"
executables = ["cc_plugins_test_tool.exe", "cc_plugins_test_tool"]
homepage = "https://example.org/helper"
version_args = ["--version"]
"#;

const MAIN: &str = r#"
ccad.command { name = "run", kind = "query",
  params = { properties = {
    args = { type = "array" }, files = { type = "object" }, outputs = { type = "array" },
    timeout = { type = "number" }, copy = { type = "object" }, tool = { type = "string" },
    weather = { type = "string", ui = "open_path" },
  } },
  run = function(a)
    local copy = a.copy
    if a.weather then copy = { ["in.epw"] = a.weather } end
    return ccad.process.run(a.tool or "helper",
      { args = a.args, files = a.files, outputs = a.outputs, timeout = a.timeout, copy = copy })
  end }
ccad.command { name = "walls",
  params = { properties = { args = { type = "array" } } },
  run = function(a)
    ccad.run("wall_create", { start = { 0, 0 }, ["end"] = { 4, 0 } })
    local r = ccad.process.run("helper", { args = a.args, outputs = { "out.txt" } })
    ccad.run("wall_create", { start = { 0, 2 }, ["end"] = { 4, 2 } })
    return { text = r.files["out.txt"], code = r.code }
  end }
ccad.command { name = "spin", kind = "query",
  params = { properties = { n = { type = "integer" }, args = { type = "array" } } },
  run = function(a)
    for _ = 1, a.n do end
    local r = ccad.process.run("helper", { args = a.args })
    for _ = 1, a.n do end
    return r.code
  end }
ccad.command { name = "misuse", kind = "query",
  params = { properties = { how = { type = "string" } } },
  run = function(a)
    if a.how == "coroutine" then
      return coroutine.wrap(function() return ccad.process.run("helper", { args = { "exit", "0" } }) end)()
    elseif a.how == "yield" then
      coroutine.yield("mine")
    elseif a.how == "pcall" then
      local ok, r = pcall(ccad.process.run, "helper", { args = { "out", "inside pcall" } })
      return { ok = ok, out = r.stdout }
    elseif a.how == "pcall_error" then
      local ok, e = pcall(ccad.process.run, "helper", { args = { "exit", "0" }, outputs = { "../x" } })
      return { ok = ok, message = tostring(e) }
    elseif a.how == "sort" then
      local t = { 2, 1 }
      table.sort(t, function(x, y) ccad.process.run("helper", { args = { "exit", "0" } }); return x < y end)
    end
  end }
"#;

/// A fresh temporary folder.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ccad-process-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The test plugin `id` (api 2) in `dir/<id>` with `extra` in its manifest.
fn plugin_dir(dir: &Path, id: &str, extra: &str, main: &str) -> PathBuf {
    let p = dir.join(id);
    std::fs::create_dir_all(p.join("locales")).unwrap();
    let manifest = format!("id = \"{id}\"\nname = \"Plugin P\"\nversion = \"1.0\"\napi = 2\n{extra}");
    std::fs::write(p.join("plugin.toml"), manifest).unwrap();
    std::fs::write(p.join("main.lua"), main).unwrap();
    std::fs::write(p.join("locales").join("en.toml"), "[tool]\nhelper = \"Helper tool\"\n").unwrap();
    p
}

struct Setup {
    session: Session,
    dir: PathBuf,
}

/// A session with the plugin `id`, granted `tools` (and `files`) as asked,
/// its tool set to the test program when `configured`.
fn setup_with(name: &str, id: &str, granted: Permissions, configured: bool, limits: Limits) -> Setup {
    let dir = scratch(name);
    let extra = format!("permissions = [\"tools\", \"files\"]\n{TOOLS}");
    let mut plugin = Plugin::load(&plugin_dir(&dir, id, &extra, MAIN), limits).unwrap();
    plugin.grant(granted);
    if configured {
        plugin.set_tool_path("helper", Some(PathBuf::from(TOOL))).unwrap();
    }
    let mut host = PluginHost::new();
    host.add(plugin).unwrap();
    let mut session = Session::default();
    session.set_extension(Some(Box::new(host)));
    Setup { session, dir }
}

fn tools_only() -> Permissions {
    Permissions { tools: true, ..Permissions::NONE }
}

fn setup(name: &str) -> Setup {
    setup_with(name, "p", tools_only(), true, Limits::default())
}

fn run(s: &mut Session, args: Value) -> cc_engine::Result<Value> {
    s.execute("p__run", args)
}

fn path(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Poll the session until its pending command ends.
fn finish(s: &mut Session) -> cc_engine::Result<Value> {
    let start = Instant::now();
    loop {
        if let Some(result) = s.poll_job() {
            return result;
        }
        assert!(start.elapsed() < Duration::from_secs(60), "the job never ended");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn walls(s: &mut Session) -> usize {
    s.execute("elements_list", json!({"category": "wall"})).unwrap().as_array().unwrap().len()
}

#[test]
fn the_permission_and_a_chosen_program_are_needed() {
    let mut no = setup_with("perm", "p", Permissions::NONE, true, Limits::default());
    let e = run(&mut no.session, json!({"args": ["exit", "0"]})).unwrap_err();
    assert_eq!(e.code, ErrorCode::Script);
    assert!(e.message.contains("\"tools\" permission"), "{}", e.message);

    let mut unset = setup_with("unset", "p", tools_only(), false, Limits::default());
    let e = run(&mut unset.session, json!({"args": ["exit", "0"]})).unwrap_err();
    assert!(e.message.contains("no program was chosen for Helper tool (helper)"), "{}", e.message);
    assert!(e.message.contains("ccad plugins tools <folder> p helper=<path>"), "{}", e.message);
    assert!(e.message.contains("--tool p.helper=<path>"), "{}", e.message);

    let mut ok = setup("unknown");
    let e = run(&mut ok.session, json!({"tool": "other"})).unwrap_err();
    assert!(e.message.contains("no tool \"other\""), "{}", e.message);
    let _ = std::fs::remove_dir_all(&no.dir);
    let _ = std::fs::remove_dir_all(&unset.dir);
    let _ = std::fs::remove_dir_all(&ok.dir);
}

#[test]
fn a_program_not_named_as_the_manifest_says_is_refused() {
    let dir = scratch("names");
    let extra = format!("permissions = [\"tools\"]\n{TOOLS}");
    let mut plugin = Plugin::load(&plugin_dir(&dir, "p", &extra, MAIN), Limits::default()).unwrap();
    plugin.grant(tools_only());
    let other = dir.join("other_program.exe");
    std::fs::copy(TOOL, &other).unwrap();
    let e = plugin.set_tool_path("helper", Some(other.clone())).unwrap_err();
    assert!(e.contains("must be named cc_plugins_test_tool.exe or cc_plugins_test_tool"), "{e}");
    assert!(plugin.set_tool_path("helper", Some(dir.join("missing").join("cc_plugins_test_tool"))).is_err());
    assert!(plugin.set_tool_path("helper", Some(PathBuf::from("cc_plugins_test_tool"))).is_err(), "relative");
    assert!(plugin.set_tool_path("nope", Some(PathBuf::from(TOOL))).unwrap_err().contains("no tool \"nope\""));
    // A stored path is checked when it runs.
    plugin.set_tool_paths(BTreeMap::from([("helper".to_string(), other)]));
    assert!(plugin.tools()[0].problem().unwrap().contains("must be named"));
    let mut host = PluginHost::new();
    host.add(plugin).unwrap();
    let mut s = Session::default();
    s.set_extension(Some(Box::new(host)));
    let e = run(&mut s, json!({"args": ["exit", "0"]})).unwrap_err();
    assert!(e.message.contains("cannot run") && e.message.contains("must be named"), "{}", e.message);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn arguments_reach_the_program_as_they_are() {
    let mut t = setup("args");
    let args = ["a & b", "x; rm -rf /", "$(whoami)", "`id`", "\"quoted\"", "with space", "%PATH%", "|", ">out", ""];
    let mut all = vec!["args"];
    all.extend(args);
    let out = run(&mut t.session, json!({ "args": all })).unwrap();
    assert_eq!(out["code"], json!(0), "{out}");
    let seen: Vec<String> = serde_json::from_str(out["stdout"].as_str().unwrap().trim()).unwrap();
    assert_eq!(seen, args.map(str::to_string));
    assert_eq!((out["truncated"].clone(), out["timed_out"].clone()), (json!(false), json!(false)));
    let _ = std::fs::remove_dir_all(&t.dir);
}

#[test]
fn files_go_in_and_outputs_come_back() {
    let mut t = setup_with("roundtrip", "p", tools_only(), true, Limits::default());
    let weather = t.dir.join("weather.epw");
    std::fs::write(&weather, "LOCATION,Somewhere").unwrap();
    let out = run(
        &mut t.session,
        json!({
            "weather": path(&weather),
            "files": { "in.idf": "Building,House;", "sub/notes.txt": "deep" },
            "args": [
                "copy", "in.idf", "result.csv",
                "write", "out/table.htm", "<table/>",
                "copy", "in.epw", "out/weather.copy",
                "write", "eplusout.err", "warnings",
                "write", "sql.err", "more",
                "write", "keep.log", "not asked for",
                "copy", "sub/notes.txt", "sub/notes.out",
            ],
            "outputs": ["result.csv", "out/*", "*.err", "missing.txt", "sub/*.out"],
        }),
    )
    .unwrap();
    assert_eq!(out["code"], json!(0), "{out}");
    let files = out["files"].as_object().unwrap();
    let mut names: Vec<&str> = files.keys().map(String::as_str).collect();
    names.sort();
    assert_eq!(
        names,
        ["eplusout.err", "out/table.htm", "out/weather.copy", "result.csv", "sql.err", "sub/notes.out"],
        "inputs, files not asked for and missing ones are not read back"
    );
    assert_eq!(files["result.csv"], json!("Building,House;"));
    assert_eq!(files["out/weather.copy"], json!("LOCATION,Somewhere"));
    assert_eq!(files["sub/notes.out"], json!("deep"));
    // Only a path granted to the run may be copied.
    let other = t.dir.join("secret.txt");
    std::fs::write(&other, "x").unwrap();
    let e = run(&mut t.session, json!({ "copy": { "in.epw": path(&other) }, "args": ["exit", "0"] })).unwrap_err();
    assert!(e.message.contains("not a file given to this command"), "{}", e.message);
    let _ = std::fs::remove_dir_all(&t.dir);
}

#[test]
fn names_outside_the_run_folder_are_refused() {
    let mut t = setup("badnames");
    let cases = [
        json!({ "files": { "../escape.txt": "x" } }),
        json!({ "files": { "/abs.txt": "x" } }),
        json!({ "files": { "C:/drive.txt": "x" } }),
        json!({ "files": { "a\\b.txt": "x" } }),
        json!({ "files": { "CON": "x" } }),
        json!({ "files": { "nul.txt": "x" } }),
        json!({ "files": { "a/../../b": "x" } }),
        json!({ "outputs": ["../*"] }),
        json!({ "outputs": ["/etc/*"] }),
        json!({ "outputs": [""] }),
        json!({ "files": { "same.txt": "x" }, "copy": { "same.txt": "y" } }),
        json!({ "args": "not a list" }),
        json!({ "timeout": 0 }),
        json!({ "timeout": 7201 }),
    ];
    for args in cases {
        let e = run(&mut t.session, args.clone()).unwrap_err();
        assert!(e.message.contains("ccad.process.run"), "{args}: {}", e.message);
    }
    let _ = std::fs::remove_dir_all(&t.dir);
}

#[test]
fn sizes_are_limited() {
    let limits = Limits { memory: 512 * 1024 * 1024, ..Limits::default() };
    let mut t = setup_with("limits", "p", tools_only(), true, limits);
    // Output: 3 MiB written, 1 MiB kept.
    let out = run(&mut t.session, json!({ "args": ["spam", "3145728", "err", "after"] })).unwrap();
    assert_eq!(out["stdout"].as_str().unwrap().len(), 1024 * 1024);
    assert_eq!(out["truncated"], json!(true));
    assert_eq!(out["stderr"].as_str().unwrap().trim(), "after");
    // An output file past 64 MiB.
    let e = run(&mut t.session, json!({ "args": ["big", "huge.bin", "67108865"], "outputs": ["*.bin"] })).unwrap_err();
    assert!(e.message.contains("huge.bin is larger than 64 MiB"), "{}", e.message);
    // Inputs past 256 MiB: 17 names for one 16 MiB string.
    let src = r#"
      local s = string.rep("x", 16 * 1024 * 1024)
      local files = {}
      for i = 1, 17 do files["f" .. i] = s end
      return ccad.process.run("helper", { files = files })"#;
    let dir = scratch("limits-in");
    let main = format!("ccad.command {{ name = \"inputs\", kind = \"query\", run = function() {src} end }}");
    let extra = format!("permissions = [\"tools\"]\n{TOOLS}");
    let mut plugin = Plugin::load(&plugin_dir(&dir, "q", &extra, &main), limits).unwrap();
    plugin.grant(tools_only());
    plugin.set_tool_path("helper", Some(PathBuf::from(TOOL))).unwrap();
    let mut host = PluginHost::new();
    host.add(plugin).unwrap();
    let mut s = Session::default();
    s.set_extension(Some(Box::new(host)));
    let e = s.execute("q__inputs", json!({})).unwrap_err();
    assert!(e.message.contains("the input files take more than 256 MiB"), "{}", e.message);
    let _ = std::fs::remove_dir_all(&t.dir);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn timeouts_exit_codes_and_the_run_folder() {
    let mut t = setup_with("codes", "pt", tools_only(), true, Limits::default());
    let run = |s: &mut Session, args: Value| s.execute("pt__run", args);
    let start = Instant::now();
    let out = run(&mut t.session, json!({ "args": ["out", "started", "sleep", "20000"], "timeout": 0.5 })).unwrap();
    assert!(start.elapsed() < Duration::from_secs(15), "{:?}", start.elapsed());
    assert_eq!(out["timed_out"], json!(true));
    assert!(out.get("code").is_none(), "no exit code: {out}");
    assert_eq!(out["stdout"].as_str().unwrap().trim(), "started");

    let out = run(&mut t.session, json!({ "args": ["err", "boom", "exit", "3"] })).unwrap();
    assert_eq!((out["code"].clone(), out["stderr"].as_str().unwrap().trim()), (json!(3), "boom"));
    let out = run(&mut t.session, json!({ "args": ["nonsense"] })).unwrap();
    assert_eq!(out["code"], json!(101));

    // The run folder is the working folder, fresh, and gone afterwards.
    let out = run(&mut t.session, json!({ "args": ["cwd"] })).unwrap();
    let folder = PathBuf::from(out["stdout"].as_str().unwrap().trim());
    let name = folder.file_name().unwrap().to_string_lossy().into_owned();
    assert!(name.starts_with("constructcraft-pt-") && name.len() > "constructcraft-pt-".len() + 8, "{name}");
    assert!(!folder.exists(), "{} was deleted", folder.display());
    let out = run(&mut t.session, json!({ "args": ["cwd"] })).unwrap();
    assert_ne!(PathBuf::from(out["stdout"].as_str().unwrap().trim()), folder, "each run has its own folder");
    // Also after a failure (an output too large).
    let e = run(&mut t.session, json!({ "args": ["big", "huge.bin", "67108865", "cwd"], "outputs": ["*.bin"] }));
    assert!(e.is_err());
    let leftovers: Vec<_> = std::fs::read_dir(std::env::temp_dir())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("constructcraft-pt-"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    let _ = std::fs::remove_dir_all(&t.dir);
}

#[test]
fn the_environment_is_kept_to_a_few_variables() {
    let mut t = setup("env");
    let out = run(&mut t.session, json!({ "args": ["env"] })).unwrap();
    let names: Vec<String> = out["stdout"].as_str().unwrap().lines().map(|l| l.trim().to_uppercase()).collect();
    assert!(names.contains(&"PATH".to_string()), "{names:?}");
    let allowed = ["PATH", "SYSTEMROOT", "TEMP", "TMP", "HOME", "USERPROFILE", "LANG"];
    // Windows adds a few of its own to every process.
    let added = ["SYSTEMDRIVE", "WINDIR", "COMSPEC", "PATHEXT", "PROCESSOR_ARCHITECTURE"];
    for name in &names {
        assert!(
            allowed.contains(&name.as_str())
                || name.starts_with("LC_")
                || (cfg!(windows) && added.contains(&name.as_str())),
            "{name} was passed on: {names:?}"
        );
    }
    assert!(!names.iter().any(|n| n.starts_with("CARGO")), "{names:?}");
    let _ = std::fs::remove_dir_all(&t.dir);
}

#[test]
fn a_non_blocking_session_leaves_the_command_pending() {
    let mut t = setup_with("pending", "pp", tools_only(), true, Limits::default());
    let s = &mut t.session;
    s.set_blocking(false);
    let depth = s.doc.undo_depth();
    let args = json!({ "args": ["lines", "3", "50", "write", "out.txt", "from the program"] });
    let e = s.execute("pp__walls", args.clone()).unwrap_err();
    assert_eq!(e.code, ErrorCode::Pending, "{}", e.message);
    let job = s.job().unwrap();
    assert_eq!(job.command, "pp__walls");
    assert_eq!((job.info.plugin.as_str(), job.info.plugin_name.as_str()), ("pp", "Plugin P"));
    assert_eq!(job.info.tool, "Helper tool");
    assert_eq!(walls(s), 1, "the first wall is there meanwhile");
    let e = s.execute("wall_create", json!({"start": [9, 9], "end": [10, 9]})).unwrap_err();
    assert!(e.message.contains("a plugin command is running"), "{}", e.message);
    let out = finish(s).unwrap();
    assert_eq!(out, json!({ "text": "from the program", "code": 0 }));
    assert_eq!((walls(s), s.doc.undo_depth()), (2, depth + 1), "one undo step");
    s.execute("undo", json!({})).unwrap();
    assert_eq!(walls(s), 0);

    // Blocking: the same result at once.
    s.set_blocking(true);
    assert_eq!(s.execute("pp__walls", args).unwrap(), out);
    assert_eq!(walls(s), 2);
    s.execute("undo", json!({})).unwrap();

    // Cancelled: the program stops, nothing stays, the run folder goes.
    s.set_blocking(false);
    let start = Instant::now();
    let e = s.execute("pp__walls", json!({ "args": ["lines", "1000", "20"] })).unwrap_err();
    assert_eq!(e.code, ErrorCode::Pending);
    while s.job().unwrap().info.last_line.is_empty() {
        assert!(start.elapsed() < Duration::from_secs(30));
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(s.job().unwrap().info.last_line.starts_with("line "), "{:?}", s.job());
    let e = s.cancel_job().unwrap().unwrap_err();
    assert_eq!(e.code, ErrorCode::Cancelled);
    assert!(start.elapsed() < Duration::from_secs(15), "{:?}", start.elapsed());
    assert_eq!((walls(s), s.doc.undo_depth()), (0, depth));
    let leftovers: Vec<_> = std::fs::read_dir(std::env::temp_dir())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("constructcraft-pp-"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    // The plugin works again afterwards.
    s.set_blocking(true);
    assert_eq!(s.execute("pp__walls", json!({ "args": ["write", "out.txt", "again"] })).unwrap()["text"], "again");
    let _ = std::fs::remove_dir_all(&t.dir);
}

#[test]
fn waiting_does_not_count_against_the_time_limit_but_instructions_do() {
    let short = Limits { time: Duration::from_millis(1500), ..Limits::default() };
    let mut t = setup_with("clock", "p", tools_only(), true, short);
    let out = t.session.execute("p__spin", json!({ "n": 1, "args": ["sleep", "2500"] })).unwrap();
    assert_eq!(out, json!(0), "the wait did not count");

    let small = Limits { instructions: 3_000_000, ..Limits::default() };
    let mut t2 = setup_with("budget", "p", tools_only(), true, small);
    let ok = t2.session.execute("p__spin", json!({ "n": 1_000_000, "args": ["exit", "0"] }));
    assert_eq!(ok.unwrap(), json!(0));
    // Each half alone fits; both do not: the budget goes on across the wait.
    let e = t2.session.execute("p__spin", json!({ "n": 2_000_000, "args": ["exit", "0"] })).unwrap_err();
    assert!(e.message.contains("instruction limit"), "{}", e.message);
    let _ = std::fs::remove_dir_all(&t.dir);
    let _ = std::fs::remove_dir_all(&t2.dir);
}

#[test]
fn only_a_command_itself_waits() {
    let mut t = setup("misuse");
    let s = &mut t.session;
    let e = s.execute("p__misuse", json!({ "how": "coroutine" })).unwrap_err();
    assert!(e.message.contains("only a command's own run function"), "{}", e.message);
    let e = s.execute("p__misuse", json!({ "how": "yield" })).unwrap_err();
    assert!(e.message.contains("coroutine.yield outside"), "{}", e.message);
    let e = s.execute("p__misuse", json!({ "how": "sort" })).unwrap_err();
    assert_eq!(e.code, ErrorCode::Script, "{}", e.message);
    // pcall lets the command wait, and catches the call's own errors.
    let out = s.execute("p__misuse", json!({ "how": "pcall" })).unwrap();
    assert_eq!(out["ok"], json!(true));
    assert_eq!(out["out"].as_str().unwrap().trim(), "inside pcall");
    let out = s.execute("p__misuse", json!({ "how": "pcall_error" })).unwrap();
    assert_eq!(out["ok"], json!(false));
    assert!(out["message"].as_str().unwrap().contains("`.` and `..`"), "{out}");
    let _ = std::fs::remove_dir_all(&t.dir);
}

#[test]
fn not_while_loading_nor_in_event_handlers() {
    let dir = scratch("handlers");
    let extra = format!("permissions = [\"tools\"]\n{TOOLS}");
    let loading = "ccad.process.run('helper', {})";
    let e = Plugin::load(&plugin_dir(&dir, "p", &extra, loading), Limits::default()).unwrap_err();
    assert!(e.message.contains("ccad.process.run is only available while"), "{}", e.message);

    let handler = r#"ccad.on("model_changed", function() ccad.process.run("helper", { args = { "exit", "0" } }) end)"#;
    let mut plugin = Plugin::load(&plugin_dir(&dir, "h", &extra, handler), Limits::default()).unwrap();
    plugin.grant(tools_only());
    plugin.set_tool_path("helper", Some(PathBuf::from(TOOL))).unwrap();
    let mut host = PluginHost::new();
    host.add(plugin).unwrap();
    let mut s = Session::default();
    s.set_extension(Some(Box::new(host)));
    s.execute("wall_create", json!({"start": [0, 0], "end": [3, 0]})).unwrap();
    let messages = s.take_extension_messages();
    assert!(messages.iter().any(|m| m.contains("not available in event handlers")), "{messages:?}");
    // Scripts have no ccad.process.
    let out = cc_plugins::run_script(&mut s, "return ccad.process == nil", "t", json!({}), Default::default());
    assert_eq!(out.result.unwrap(), json!(true));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn manifests_declare_tools_with_the_permission() {
    let dir = scratch("manifest");
    let load = |extra: &str| Plugin::load(&plugin_dir(&dir, "m", extra, ""), Limits::default());
    let e = load(TOOLS).unwrap_err();
    assert!(e.message.contains("tools need the \"tools\" permission"), "{}", e.message);
    let head = "id = \"m\"\nname = \"M\"\nversion = \"1\"\napi = 1\npermissions = [\"tools\"]\n";
    let e = cc_plugins::Manifest::parse(head).unwrap_err();
    assert!(e.contains("the tools permission needs api = 2"), "{e}");
    let e = cc_plugins::Manifest::parse(&format!("{}{TOOLS}", head.replace("\"tools\"", ""))).unwrap_err();
    assert!(e.contains("tools need api = 2"), "{e}");
    let many: String =
        (0..9).map(|i| format!("[[tools]]\nid = \"t{i}\"\nname = \"T\"\nexecutables = [\"t\"]\n")).collect();
    let e = load(&format!("permissions = [\"tools\"]\n{many}")).unwrap_err();
    assert!(e.message.contains("at most 8 tools"), "{}", e.message);
    let twice = "[[tools]]\nid = \"t\"\nname = \"T\"\nexecutables = [\"t\"]\n".repeat(2);
    let e = load(&format!("permissions = [\"tools\"]\n{twice}")).unwrap_err();
    assert!(e.message.contains("used twice"), "{}", e.message);
    let batch = "[[tools]]\nid = \"t\"\nname = \"T\"\nexecutables = [\"run.bat\"]\n";
    let e = load(&format!("permissions = [\"tools\"]\n{batch}")).unwrap_err();
    assert!(e.message.contains("shell"), "{}", e.message);
    let unknown = "[[tools]]\nid = \"t\"\nname = \"T\"\nexecutables = [\"t\"]\nshell = true\n";
    assert!(load(&format!("permissions = [\"tools\"]\n{unknown}")).is_err());
    // Granted only what the manifest asks for.
    let mut plugin = load(&format!("permissions = [\"tools\"]\n{TOOLS}")).unwrap();
    plugin.grant(Permissions { files: true, tools: true, ..Permissions::NONE });
    assert_eq!(plugin.granted(), tools_only());
    let tools = plugin.tools();
    assert_eq!((tools.len(), tools[0].name.as_str(), tools[0].path.is_none()), (1, "Helper tool", true));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stored_tool_paths_are_hostile_input() {
    use cc_plugins::tools::{MAX_TOOLS_FILE, read_tool_paths, tools_path, write_tool_paths};
    let folder = scratch("stored");
    let absolute = PathBuf::from(TOOL);
    assert!(read_tool_paths(&folder, "p").is_empty(), "no file");
    write_tool_paths(&folder, "p", &BTreeMap::from([("helper".to_string(), absolute.clone())])).unwrap();
    assert_eq!(read_tool_paths(&folder, "p"), BTreeMap::from([("helper".to_string(), absolute.clone())]));
    let names: Vec<String> = std::fs::read_dir(folder.join(".tools"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["p.json"], "no temporary file left");
    let file = tools_path(&folder, "p");
    for bad in ["not json", "[1, 2]", "\"text\"", "null", "{\"helper\": 3}", "{\"helper\": \"relative/tool\"}"] {
        std::fs::write(&file, bad).unwrap();
        assert!(read_tool_paths(&folder, "p").is_empty(), "{bad}");
    }
    let mixed = json!({ "helper": path(&absolute), "Bad Id": path(&absolute), "x": ["a"] });
    std::fs::write(&file, mixed.to_string()).unwrap();
    assert_eq!(read_tool_paths(&folder, "p").len(), 1);
    std::fs::write(&file, format!("{{\"helper\": \"{}\"}}", "x".repeat(MAX_TOOLS_FILE as usize))).unwrap();
    assert!(read_tool_paths(&folder, "p").is_empty(), "too large");
    assert!(read_tool_paths(&folder, "../p").is_empty(), "a bad id");
    assert!(write_tool_paths(&folder, "../p", &BTreeMap::new()).is_err());
    // No paths: the file goes.
    write_tool_paths(&folder, "p", &BTreeMap::new()).unwrap();
    assert!(!file.exists());

    // A folder of plugins loads them; a stored path of the wrong name is
    // kept but refused when it runs.
    let extra = format!("permissions = [\"tools\"]\n{TOOLS}");
    plugin_dir(&folder, "p", &extra, MAIN);
    write_tool_paths(&folder, "p", &BTreeMap::from([("helper".to_string(), absolute.clone())])).unwrap();
    let mut host = PluginHost::new();
    assert!(host.load_folder(&folder, Limits::default(), |m| Some(m.requested())).is_empty());
    assert_eq!(host.plugin("p").unwrap().tools()[0].path.as_deref(), Some(absolute.as_path()));
    let summary = host.summary();
    assert_eq!(summary[0]["tools"][0]["id"], json!("helper"));
    assert_eq!(summary[0]["tools"][0]["problem"], Value::Null);
    assert_eq!(summary[0]["granted"]["tools"], json!(true));
    let mut s = Session::default();
    s.set_extension(Some(Box::new(host)));
    assert_eq!(s.execute("p__run", json!({ "args": ["exit", "4"] })).unwrap()["code"], json!(4));
    // Removing the plugin removes its paths.
    cc_plugins::store::uninstall(&folder, "p").unwrap();
    assert!(!tools_path(&folder, "p").exists());
    let _ = std::fs::remove_dir_all(&folder);
}

#[test]
fn test_runs_the_version_arguments() {
    let dir = scratch("version");
    let extra = format!("permissions = [\"tools\"]\n{TOOLS}");
    let plugin = Plugin::load(&plugin_dir(&dir, "p", &extra, MAIN), Limits::default()).unwrap();
    let tool = plugin.tools().remove(0);
    let answer = cc_plugins::tools::test("p", &tool, Path::new(TOOL)).unwrap();
    assert_eq!(answer, "cc_plugins_test_tool 1.0");
    assert!(cc_plugins::tools::test("p", &tool, &dir.join("cc_plugins_test_tool")).is_err(), "missing");
    assert_eq!(cc_plugins::tools::find_on_path(&["surely-no-such-program-cc".to_string()]), None);
    let _ = std::fs::remove_dir_all(&dir);
}
