//! The `ccad` table scripts and plugins see.
//!
//! - `ccad.run(name, args)`: run an engine command; returns its result, or
//!   raises an error.
//! - `ccad.try(name, args)`: the same, returning `result` or `nil, err`
//!   (`err.code`, `err.message`) instead of raising.
//! - `ccad.commands()`: the commands (name, kind, group, description);
//!   `ccad.describe(name)` one of them with its parameters' `schema`.
//! - `ccad.null` (JSON null), `ccad.array(t)` (marks `t`, even empty, as a
//!   list), `ccad.json.encode(v)` / `ccad.json.decode(s)`, `ccad.version`,
//!   `ccad.api` (the plugin API version).
//! - In plugins only (see [`crate::plugin`]): `ccad.plugin`, `ccad.command`
//!   and `ccad.on` (while the plugin loads), `ccad.settings`, `ccad.data`,
//!   `ccad.file` and `ccad.process`.
//!
//! What a plugin command may do is not decided here: it runs inside a
//! [`cc_engine::PluginScope`], which [`Session::execute`] checks for every
//! command run during it (also by graphs, scripts or macros it starts). The
//! user's scripts run unrestricted.
//!
//! `ccad.file.read(path)` and `ccad.file.write(path, data)` (plugins only,
//! while a command runs) read and write the files the command's arguments
//! grant ([`FileRun`]): an `open_path` parameter's file to read, a
//! `save_path` parameter's file to write.
//!
//! `ccad.process.run(tool, options)` (plugins only, in a command's own
//! coroutine, with the `tools` permission) checks the request
//! ([`ProcessRun`]) and yields; the plugin host runs the program on a
//! background thread ([`crate::process`]) and resumes the command with its
//! result table, or with an error raised at the call.
//!
//! The functions that reach the session exist only while a script or a
//! plugin command runs: the Lua wrappers stay (a plugin may keep them in
//! locals when it loads) and call the session's functions through a private
//! table filled for the run.

use crate::convert::{from_lua, to_lua};
use crate::process::{DEFAULT_TIMEOUT, Finished, MAX_INPUTS, MAX_TIMEOUT, Request, check_name};
use crate::tools::Tool;
use cc_engine::{EngineError, ErrorCode, Grants, Session};
use mlua::{Lua, LuaSerdeExt, Table, Value as LuaValue};
use serde_json::Value;
use std::cell::RefCell;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The plugin API version this build provides (`api` in `plugin.toml`).
pub const API_VERSION: u32 = 3;

/// Makes the wrappers over the private table of the run's functions;
/// returns its `get`.
const PRELUDE: &str = r#"
local ccad, impl = ...
local error = error
local function get(name, shown)
  local f = impl[name]
  if not f then error("ccad." .. (shown or name) .. " is only available while a script or plugin command runs", 3) end
  return f
end
function ccad.run(name, args) return get("run")(name, args) end
function ccad.try(name, args) return get("try")(name, args) end
function ccad.commands() return get("commands")() end
function ccad.describe(name) return get("describe")(name) end
return get
"#;

/// `ccad.file`, for plugins.
const FILE_PRELUDE: &str = r#"
local ccad, get = ...
local file = {}
function file.read(path) return get("file_read", "file.read")(path) end
function file.write(path, data) return get("file_write", "file.write")(path, data) end
ccad.file = file
"#;

/// `ccad.process`, for plugins: the request is checked by the host's
/// function, then the command's coroutine yields its token and is resumed
/// with the result (or a message to raise).
const PROCESS_PRELUDE: &str = r#"
local ccad, get = ...
local yield, error = coroutine.yield, error
local process = {}
function process.run(tool, options)
  local token = get("process_run", "process.run")(tool, options)
  local result, message = yield(token)
  if message ~= nil then error(message, 2) end
  return result
end
ccad.process = process
"#;

/// `ccad.http`, for API 3 plugins with the `network` permission and `allowed_hosts`.
const HTTP_PRELUDE: &str = r#"
local ccad, get = ...
local http = {}
function http.get(url, options) return get("http_get", "http.get")(url, options) end
function http.download(url, key, options) return get("http_download", "http.download")(url, key, options) end
function http.cached(key, options) return get("http_cached", "http.cached")(key, options) end
ccad.http = http
"#;

/// Most arguments, input files, copies and output patterns of one
/// `ccad.process.run`, and the longest argument.
const MAX_ARGS: usize = 4096;
const MAX_ARG: usize = 32 * 1024;
const MAX_INPUT_FILES: usize = 1024;
const MAX_PATTERNS: usize = 256;

/// Largest file `ccad.file.read` reads, in bytes.
pub const MAX_FILE_READ: u64 = 64 * 1024 * 1024;
/// Most bytes `ccad.file.write` takes in one command run, all files together.
pub const MAX_FILE_WRITTEN: u64 = 256 * 1024 * 1024;

/// The files a plugin command's arguments grant, and what `ccad.file.write`
/// gave for them: kept in memory and written ([`FileRun::commit`]) only
/// when the command succeeds.
#[derive(Debug, Default)]
pub struct FileRun {
    grants: Grants,
    /// Each written file's key ([`cc_engine::files::write_key`]) and its
    /// content (a later write to the same file replaces it).
    pending: Vec<(PathBuf, Vec<u8>)>,
    /// Bytes given to `ccad.file.write` so far.
    written: u64,
}

impl FileRun {
    pub fn new(grants: Grants) -> FileRun {
        FileRun { grants, pending: Vec::new(), written: 0 }
    }

    /// Whether `path` is a file to read given to the command.
    pub(crate) fn may_read(&self, path: &Path) -> bool {
        self.grants.may_read(path)
    }

    /// Read a granted file to read.
    fn read(&self, path: &Path) -> Result<Vec<u8>, EngineError> {
        if !self.grants.may_read(path) {
            return Err(EngineError::rejected(format!(
                "ccad.file.read: {} is not a file to read given to this command (an open_path parameter)",
                path.display()
            )));
        }
        let io = |e: std::io::Error| EngineError::io(format!("{}: {e}", path.display()));
        let too_large = || {
            EngineError::invalid(format!(
                "ccad.file.read: {} is larger than {} MiB",
                path.display(),
                MAX_FILE_READ >> 20
            ))
        };
        let file = std::fs::File::open(path).map_err(io)?;
        if file.metadata().map_err(io)?.len() > MAX_FILE_READ {
            return Err(too_large());
        }
        let mut bytes = Vec::new();
        file.take(MAX_FILE_READ + 1).read_to_end(&mut bytes).map_err(io)?;
        if bytes.len() as u64 > MAX_FILE_READ {
            return Err(too_large());
        }
        Ok(bytes)
    }

    /// Keep `bytes` to write to a granted file to write.
    fn write(&mut self, path: &Path, bytes: Vec<u8>) -> Result<(), EngineError> {
        if !self.grants.may_write(path) {
            return Err(EngineError::rejected(format!(
                "ccad.file.write: {} is not a file to write given to this command (a save_path parameter)",
                path.display()
            )));
        }
        let key = cc_engine::files::write_key(path).map_err(EngineError::invalid)?;
        let total = self.written.saturating_add(bytes.len() as u64);
        if total > MAX_FILE_WRITTEN {
            return Err(too_much_written());
        }
        self.written = total;
        match self.pending.iter_mut().find(|(k, _)| *k == key) {
            Some((_, old)) => *old = bytes,
            None => self.pending.push((key, bytes)),
        }
        Ok(())
    }

    /// Write the files kept: each to a temporary file in its folder first,
    /// then all renamed into place, so a failure while writing changes none
    /// of them.
    pub fn commit(self) -> Result<(), EngineError> {
        let mut temps: Vec<(PathBuf, &PathBuf)> = Vec::new();
        let clean = |temps: &[(PathBuf, &PathBuf)]| {
            for (temp, _) in temps {
                let _ = std::fs::remove_file(temp);
            }
        };
        for (i, (key, bytes)) in self.pending.iter().enumerate() {
            let name = key.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let temp = key.with_file_name(format!(".{name}.{}-{i}.tmp", std::process::id()));
            if let Err(e) = std::fs::write(&temp, bytes) {
                let _ = std::fs::remove_file(&temp);
                clean(&temps);
                return Err(EngineError::io(format!("{}: {e}", key.display())));
            }
            temps.push((temp, key));
        }
        for (n, (temp, key)) in temps.iter().enumerate() {
            if let Err(e) = std::fs::rename(temp, key) {
                clean(temps.get(n..).unwrap_or_default());
                return Err(EngineError::io(format!("{}: {e}", key.display())));
            }
        }
        Ok(())
    }
}

/// What `ccad.process.run` knows of the plugin command running.
#[derive(Debug, Default)]
pub struct ProcessRun {
    /// The plugin's id.
    pub plugin: String,
    /// It was granted the `tools` permission.
    pub allowed: bool,
    /// Its tools, with the paths the user chose.
    pub tools: Vec<Tool>,
    /// The coroutine the command runs in: the only one that may wait.
    pub thread: Option<mlua::Thread>,
    /// The program asked for: the token its yield carries, the tool's name
    /// and the request.
    pub request: Option<(Table, String, Request)>,
}

/// Network access granted to a plugin run (`ccad.http`).
#[derive(Debug, Clone, Default)]
pub struct HttpRun {
    pub allowed: bool,
    pub allowed_hosts: Vec<String>,
    pub cache_root: PathBuf,
}

/// What a plugin command's run has besides the session: its granted files,
/// external process state, and scoped HTTP access.
#[derive(Debug, Default)]
pub struct CommandRun {
    pub files: RefCell<FileRun>,
    pub process: RefCell<ProcessRun>,
    pub http: HttpRun,
}

fn process_error(message: impl std::fmt::Display) -> mlua::Error {
    mlua::Error::runtime(format!("ccad.process.run: {message}"))
}

/// A Lua string or number as text.
fn text_of(lua: &Lua, v: LuaValue, what: &str) -> mlua::Result<String> {
    match v {
        LuaValue::String(s) => {
            Ok(s.to_str().map_err(|_| process_error(format!("{what} is not UTF-8 text")))?.to_string())
        }
        v @ (LuaValue::Integer(_) | LuaValue::Number(_)) => lua
            .coerce_string(v)?
            .map(|s| s.to_string_lossy())
            .ok_or_else(|| process_error(format!("{what} is not text"))),
        other => Err(process_error(format!("{what} is a {}, not a string", other.type_name()))),
    }
}

/// The strings of a Lua list (at most `max`).
fn string_list(lua: &Lua, v: LuaValue, what: &str, max: usize) -> mlua::Result<Vec<String>> {
    let LuaValue::Table(t) = v else {
        return Err(process_error(format!("{what} is a {}, not a list", v.type_name())));
    };
    let len = t.raw_len();
    if len > max {
        return Err(process_error(format!("{what}: at most {max} items")));
    }
    (1..=len).map(|i| text_of(lua, t.raw_get(i)?, &format!("{what}[{i}]"))).collect()
}

/// The pairs `name = string` of a Lua table (at most `max`), sorted by name.
fn string_map(lua: &Lua, v: LuaValue, what: &str, max: usize) -> mlua::Result<Vec<(String, mlua::String)>> {
    let LuaValue::Table(t) = v else {
        return Err(process_error(format!("{what} is a {}, not a table of names", v.type_name())));
    };
    let mut out = Vec::new();
    for pair in t.pairs::<LuaValue, LuaValue>() {
        let (k, v) = pair?;
        let name = text_of(lua, k, &format!("a name of {what}"))?;
        let LuaValue::String(value) = v else {
            return Err(process_error(format!("{what}[\"{name}\"] is a {}, not a string", v.type_name())));
        };
        out.push((name, value));
        if out.len() > max {
            return Err(process_error(format!("{what}: at most {max} files")));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// Check a `ccad.process.run(tool, options)` call of the command whose run
/// is `ctx` (and whose granted files are `files`), and make its request and
/// the tool's name.
fn prepare(
    lua: &Lua,
    ctx: &ProcessRun,
    files: &FileRun,
    tool: &str,
    options: Option<Table>,
) -> mlua::Result<(String, Request)> {
    if ctx.thread.as_ref() != Some(&lua.current_thread()) {
        return Err(process_error(
            "only a command's own run function can run a program (not a coroutine it starts, a module being \
             required, or a function called by a library function)",
        ));
    }
    if !ctx.allowed {
        return Err(process_error(format!(
            "the plugin {} was not granted the \"tools\" permission (allow it to run external programs in the \
             Plugins window; ccad --plugins grants what the manifest asks for)",
            ctx.plugin
        )));
    }
    let Some(t) = ctx.tools.iter().find(|t| t.id == tool) else {
        let known: Vec<&str> = ctx.tools.iter().map(|t| t.id.as_str()).collect();
        return Err(process_error(format!("no tool \"{tool}\" in plugin.toml (tools: {})", known.join(", "))));
    };
    let Some(program) = t.path.clone() else {
        return Err(process_error(format!(
            "no program was chosen for {} ({}): choose {} in the Plugins window (External programs), or run \
             `ccad plugins tools <folder> {} {}=<path>`, or give --tool {}.{}=<path>",
            t.name,
            t.id,
            t.executables.join(" or "),
            ctx.plugin,
            t.id,
            ctx.plugin,
            t.id
        )));
    };
    crate::tools::check_path(&t.executables, &program)
        .map_err(|e| process_error(format!("the program chosen for {} ({}) cannot run: {e}", t.name, t.id)))?;
    let mut request = Request { plugin: ctx.plugin.clone(), program, timeout: DEFAULT_TIMEOUT, ..Request::default() };
    let mut names: Vec<String> = Vec::new();
    let mut total: u64 = 0;
    let mut add_name = |name: &str| -> mlua::Result<()> {
        check_name(name, false).map_err(process_error)?;
        let folded = if cfg!(windows) { name.to_lowercase() } else { name.to_string() };
        if names.contains(&folded) {
            return Err(process_error(format!("\"{name}\" is given twice")));
        }
        names.push(folded);
        Ok(())
    };
    let too_much = || process_error(format!("the input files take more than {} MiB", MAX_INPUTS >> 20));
    if let Some(options) = options {
        for pair in options.pairs::<LuaValue, LuaValue>() {
            let (key, value) = pair?;
            let key = match &key {
                LuaValue::String(k) => k.to_string_lossy(),
                other => return Err(process_error(format!("an option named by a {}", other.type_name()))),
            };
            match key.as_str() {
                "args" => {
                    request.args = string_list(lua, value, "args", MAX_ARGS)?;
                    if let Some(i) = request.args.iter().position(|a| a.len() > MAX_ARG || a.contains('\0')) {
                        return Err(process_error(format!(
                            "args[{}] is longer than {MAX_ARG} bytes or holds a NUL",
                            i + 1
                        )));
                    }
                }
                "files" => {
                    let list = string_map(lua, value, "files", MAX_INPUT_FILES)?;
                    // Sizes first: nothing is copied out of Lua when they are too large.
                    for (name, data) in &list {
                        add_name(name)?;
                        total = total.saturating_add(data.as_bytes().len() as u64);
                        if total > MAX_INPUTS {
                            return Err(too_much());
                        }
                    }
                    request.files.extend(list.into_iter().map(|(name, data)| (name, data.as_bytes().to_vec())));
                }
                "copy" => {
                    for (name, source) in string_map(lua, value, "copy", MAX_INPUT_FILES)? {
                        add_name(&name)?;
                        let source = PathBuf::from(source.to_string_lossy());
                        if !files.may_read(&source) {
                            return Err(process_error(format!(
                                "copy: {} is not a file given to this command to read (an open_path parameter)",
                                source.display()
                            )));
                        }
                        let size = std::fs::metadata(&source).map(|m| m.len()).unwrap_or(0);
                        total = total.saturating_add(size);
                        if total > MAX_INPUTS {
                            return Err(too_much());
                        }
                        request.copies.push((name, source));
                    }
                }
                "outputs" => {
                    request.outputs = string_list(lua, value, "outputs", MAX_PATTERNS)?;
                    for pattern in &request.outputs {
                        check_name(pattern, true).map_err(|e| process_error(format!("outputs: {e}")))?;
                    }
                }
                "timeout" => {
                    let seconds = match value {
                        LuaValue::Integer(i) => i as f64,
                        LuaValue::Number(n) => n,
                        other => {
                            return Err(process_error(format!("timeout is a {}, not seconds", other.type_name())));
                        }
                    };
                    if !(seconds > 0.0 && seconds <= MAX_TIMEOUT.as_secs_f64()) {
                        return Err(process_error(format!(
                            "timeout: more than 0 and at most {} seconds",
                            MAX_TIMEOUT.as_secs()
                        )));
                    }
                    request.timeout = Duration::from_secs_f64(seconds);
                }
                other => {
                    return Err(process_error(format!(
                        "unknown option \"{other}\" (options: args, files, copy, outputs, timeout)"
                    )));
                }
            }
        }
    }
    Ok((t.name.clone(), request))
}

/// What `ccad.process.run` returns: `{ code, stdout, stderr, truncated,
/// timed_out, files = { name = content } }` (`code` nil when there is none).
pub(crate) fn finished_table(lua: &Lua, done: &Finished) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    if let Some(code) = done.code {
        t.raw_set("code", code)?;
    }
    t.raw_set("stdout", lua.create_string(&done.stdout)?)?;
    t.raw_set("stderr", lua.create_string(&done.stderr)?)?;
    t.raw_set("truncated", done.truncated)?;
    t.raw_set("timed_out", done.timed_out)?;
    let files = lua.create_table()?;
    for (name, bytes) in &done.files {
        files.raw_set(name.as_str(), lua.create_string(bytes)?)?;
    }
    t.raw_set("files", files)?;
    Ok(t)
}

fn too_much_written() -> EngineError {
    EngineError::invalid(format!("ccad.file.write: a command writes at most {} MiB", MAX_FILE_WRITTEN >> 20))
}

/// The bytes of `ccad.file.write`'s data: a string, or a list of strings
/// joined.
fn data_bytes(data: &LuaValue) -> mlua::Result<Vec<u8>> {
    match data {
        LuaValue::String(s) => Ok(s.as_bytes().to_vec()),
        LuaValue::Table(t) => {
            let mut bytes = Vec::new();
            for part in t.sequence_values::<LuaValue>() {
                match part? {
                    LuaValue::String(s) => {
                        let part = s.as_bytes();
                        if (bytes.len() as u64).saturating_add(part.len() as u64) > MAX_FILE_WRITTEN {
                            return Err(mlua::Error::external(too_much_written()));
                        }
                        bytes.extend_from_slice(&part);
                    }
                    other => {
                        return Err(mlua::Error::runtime(format!(
                            "ccad.file.write: the list holds a {}, not a string",
                            other.type_name()
                        )));
                    }
                }
            }
            Ok(bytes)
        }
        other => Err(mlua::Error::runtime(format!(
            "ccad.file.write: the data is a {}, not a string or a list of strings",
            other.type_name()
        ))),
    }
}

/// The `ccad` table and the private table of the run's functions.
pub struct Api {
    pub ccad: Table,
    imp: Table,
    /// The prelude's `get` (reads the private table), for `ccad.file`.
    get: mlua::Function,
}

impl Api {
    /// Make the `ccad` global in `lua` for a plugin: also `ccad.file`,
    /// `ccad.process` and `ccad.http`.
    pub fn install_for_plugin(lua: &Lua) -> mlua::Result<Api> {
        let api = Api::install(lua)?;
        lua.load(FILE_PRELUDE).set_name("=ccad.file").call::<()>((api.ccad.clone(), api.get.clone()))?;
        lua.load(PROCESS_PRELUDE).set_name("=ccad.process").call::<()>((api.ccad.clone(), api.get.clone()))?;
        lua.load(HTTP_PRELUDE).set_name("=ccad.http").call::<()>((api.ccad.clone(), api.get.clone()))?;
        Ok(api)
    }

    /// Make the `ccad` global in `lua`.
    pub fn install(lua: &Lua) -> mlua::Result<Api> {
        let ccad = lua.create_table()?;
        let imp = lua.create_table()?;
        ccad.raw_set("null", LuaValue::NULL)?;
        ccad.raw_set("version", env!("CARGO_PKG_VERSION"))?;
        ccad.raw_set("api", API_VERSION)?;
        ccad.raw_set(
            "array",
            lua.create_function(|lua, t: Option<Table>| {
                let t = match t {
                    Some(t) => t,
                    None => lua.create_table()?,
                };
                t.set_metatable(Some(lua.array_metatable()))?;
                Ok(t)
            })?,
        )?;
        let json = lua.create_table()?;
        json.raw_set(
            "encode",
            lua.create_function(|lua, v: LuaValue| {
                let value = from_lua(lua, &v).map_err(mlua::Error::runtime)?;
                Ok(serde_json::to_string(&value).unwrap_or_default())
            })?,
        )?;
        json.raw_set(
            "decode",
            lua.create_function(|lua, s: mlua::String| {
                let value: Value = serde_json::from_slice(&s.as_bytes()).map_err(mlua::Error::runtime)?;
                to_lua(lua, &value)
            })?,
        )?;
        ccad.raw_set("json", json)?;
        let get: mlua::Function = lua.load(PRELUDE).set_name("=ccad").call((ccad.clone(), imp.clone()))?;
        lua.globals().raw_set("ccad", ccad.clone())?;
        Ok(Api { ccad, imp, get })
    }

    /// Run `f` with the session's functions available to the Lua code, and
    /// `ccad.file` and `ccad.process` working for `command` (a plugin command's;
    /// `None` for scripts and event handlers).
    pub fn with_session<R>(
        &self,
        lua: &Lua,
        session: &mut Session,
        command: Option<&CommandRun>,
        f: impl FnOnce(&Lua) -> mlua::Result<R>,
    ) -> mlua::Result<R> {
        let cell = RefCell::new(session);
        let cell = &cell;
        let no_http = HttpRun::default();
        let result = lua.scope(|scope| {
            let run = scope.create_function(move |lua, (name, args): (String, LuaValue)| {
                let mut session = cell.try_borrow_mut().map_err(mlua::Error::runtime)?;
                match execute(lua, &mut session, &name, &args)? {
                    Ok(v) => to_lua(lua, &v),
                    Err(e) => Err(mlua::Error::external(e)),
                }
            })?;
            let try_run = scope.create_function(move |lua, (name, args): (String, LuaValue)| {
                let mut session = cell.try_borrow_mut().map_err(mlua::Error::runtime)?;
                match execute(lua, &mut session, &name, &args)? {
                    Ok(v) => Ok((to_lua(lua, &v)?, LuaValue::Nil)),
                    Err(e) => {
                        let err = lua.create_table()?;
                        err.raw_set("code", code_name(e.code))?;
                        err.raw_set("message", e.message)?;
                        Ok((LuaValue::Nil, LuaValue::Table(err)))
                    }
                }
            })?;
            let commands = scope.create_function(move |lua, ()| {
                let session = cell.try_borrow().map_err(mlua::Error::runtime)?;
                let list: Vec<Value> = session
                    .commands()
                    .into_iter()
                    .map(|c| {
                        serde_json::json!({ "name": c.name, "kind": c.kind, "group": c.group, "description": c.description })
                    })
                    .collect();
                to_lua(lua, &Value::Array(list))
            })?;
            let describe = scope.create_function(move |lua, name: String| {
                let session = cell.try_borrow().map_err(mlua::Error::runtime)?;
                match session.describe(&name) {
                    Some(c) => to_lua(
                        lua,
                        &serde_json::json!({
                            "name": c.name, "kind": c.kind, "group": c.group,
                            "description": c.description, "schema": c.input_schema,
                        }),
                    ),
                    None => Ok(LuaValue::Nil),
                }
            })?;
            self.imp.raw_set("run", run)?;
            self.imp.raw_set("try", try_run)?;
            self.imp.raw_set("commands", commands)?;
            self.imp.raw_set("describe", describe)?;
            if let Some(command) = command {
                let files = &command.files;
                let read = scope.create_function(move |lua, path: String| {
                    let run = files.try_borrow().map_err(mlua::Error::runtime)?;
                    let bytes = run.read(Path::new(&path)).map_err(mlua::Error::external)?;
                    lua.create_string(&bytes)
                })?;
                let write = scope.create_function(move |_, (path, data): (String, LuaValue)| {
                    let bytes = data_bytes(&data)?;
                    let mut run = files.try_borrow_mut().map_err(mlua::Error::runtime)?;
                    run.write(Path::new(&path), bytes).map_err(mlua::Error::external)
                })?;
                self.imp.raw_set("file_read", read)?;
                self.imp.raw_set("file_write", write)?;
                let process = scope.create_function(move |lua, (tool, options): (String, Option<Table>)| {
                    let mut ctx = command.process.try_borrow_mut().map_err(mlua::Error::runtime)?;
                    let files = command.files.try_borrow().map_err(mlua::Error::runtime)?;
                    let (name, request) = prepare(lua, &ctx, &files, &tool, options)?;
                    let token = lua.create_table()?;
                    ctx.request = Some((token.clone(), name, request));
                    Ok(token)
                })?;
                self.imp.raw_set("process_run", process)?;
                bind_http(scope, &self.imp, &command.http)?;
            } else {
                let refused = scope.create_function(|_, _: mlua::MultiValue| -> mlua::Result<()> {
                    Err(process_error("not available in event handlers (only while a plugin command runs)"))
                })?;
                self.imp.raw_set("process_run", refused)?;
                bind_http(scope, &self.imp, &no_http)?;
            }
            let result = f(lua);
            self.imp.clear()?;
            result
        });
        // Leave nothing that points at the finished scope, even after an error.
        let _ = self.imp.clear();
        result
    }

    /// Run `f` with `ccad.http` bound (for catalog provider callbacks that do
    /// not touch a `Session`).
    pub fn with_http<R>(&self, lua: &Lua, http: &HttpRun, f: impl FnOnce(&Lua) -> mlua::Result<R>) -> mlua::Result<R> {
        let result = lua.scope(|scope| {
            bind_http(scope, &self.imp, http)?;
            let result = f(lua);
            self.imp.clear()?;
            result
        });
        let _ = self.imp.clear();
        result
    }
}

/// Bind `http_get`, `http_download` and `http_cached` into `imp` for one scoped run.
fn bind_http<'lua, 'scope>(
    scope: &'lua mlua::Scope<'lua, 'scope>,
    imp: &Table,
    http: &'scope HttpRun,
) -> mlua::Result<()> {
    let err = |msg: String| mlua::Error::runtime(format!("ccad.http: {msg}"));
    let get_fn = scope.create_function(move |lua, (url, opts): (String, Option<Table>)| {
        if !http.allowed {
            return Err(err("network permission is not granted".into()));
        }
        let max_bytes = opts
            .as_ref()
            .and_then(|o| o.get::<Option<usize>>("max_bytes").ok().flatten())
            .unwrap_or(crate::http::DEFAULT_GET_BYTES);
        let timeout = opts
            .as_ref()
            .and_then(|o| o.get::<Option<u64>>("timeout").ok().flatten())
            .unwrap_or(crate::http::DEFAULT_TIMEOUT_SECS);
        let (bytes, status) = crate::http::fetch_bytes(&url, &http.allowed_hosts, max_bytes, timeout).map_err(err)?;
        Ok((lua.create_string(&bytes)?, status))
    })?;
    let download_fn = scope.create_function(move |_, (url, key, opts): (String, String, Option<Table>)| {
        if !http.allowed {
            return Err(err("network permission is not granted".into()));
        }
        let max_bytes = opts
            .as_ref()
            .and_then(|o| o.get::<Option<usize>>("max_bytes").ok().flatten())
            .unwrap_or(crate::http::MAX_RESPONSE_BYTES);
        let timeout = opts
            .as_ref()
            .and_then(|o| o.get::<Option<u64>>("timeout").ok().flatten())
            .unwrap_or(crate::http::DEFAULT_TIMEOUT_SECS);
        let sha256 = opts.as_ref().and_then(|o| o.get::<Option<String>>("sha256").ok().flatten());
        let path = crate::http::download_cached(
            &url,
            &http.allowed_hosts,
            &http.cache_root,
            &key,
            max_bytes,
            timeout,
            sha256.as_deref(),
        )
        .map_err(err)?;
        Ok(path.to_string_lossy().into_owned())
    })?;
    let cached_fn = scope.create_function(move |_, (key, opts): (String, Option<Table>)| {
        let sha256 = opts.as_ref().and_then(|o| o.get::<Option<String>>("sha256").ok().flatten());
        let found = crate::http::cached_file(&http.cache_root, &key, sha256.as_deref()).map_err(err)?;
        Ok(found.map(|p| p.to_string_lossy().into_owned()))
    })?;
    imp.raw_set("http_get", get_fn)?;
    imp.raw_set("http_download", download_fn)?;
    imp.raw_set("http_cached", cached_fn)?;
    Ok(())
}

/// Run one command for the Lua code: `Err` for errors of the Lua side (bad
/// arguments), `Ok(Err)` for the command's own failure. What a plugin
/// command may run is checked by the session (its scope).
fn execute(lua: &Lua, session: &mut Session, name: &str, args: &LuaValue) -> mlua::Result<Result<Value, EngineError>> {
    let args = match args {
        LuaValue::Nil => Value::Object(Default::default()),
        other => from_lua(lua, other).map_err(|e| mlua::Error::runtime(format!("{name}: arguments: {e}")))?,
    };
    Ok(session.execute(name, args))
}

/// An error code as its JSON name.
pub fn code_name(code: ErrorCode) -> String {
    serde_json::to_value(code).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
}

/// Longest error message kept from a Lua error (tracebacks can be long).
const MAX_MESSAGE: usize = 4000;

/// A Lua error as an engine error: a command's error raised through
/// `ccad.run` keeps its code; anything else is a script error.
pub fn engine_error(e: &mlua::Error) -> EngineError {
    fn find(e: &mlua::Error) -> Option<EngineError> {
        match e {
            mlua::Error::CallbackError { cause, .. } | mlua::Error::WithContext { cause, .. } => find(cause),
            mlua::Error::ExternalError(x) => x.downcast_ref::<EngineError>().cloned(),
            _ => None,
        }
    }
    if let Some(found) = find(e) {
        return found;
    }
    let mut message = match e {
        mlua::Error::RuntimeError(m) | mlua::Error::SyntaxError { message: m, .. } => m.clone(),
        mlua::Error::MemoryError(_) => "the script used too much memory".to_string(),
        other => other.to_string(),
    };
    // The message, without the stack traceback Lua appends.
    if let Some(cut) = message.find("\nstack traceback:") {
        message.truncate(cut);
    }
    if message.len() > MAX_MESSAGE {
        let mut cut = MAX_MESSAGE;
        while !message.is_char_boundary(cut) {
            cut -= 1;
        }
        message.truncate(cut);
        message.push('…');
    }
    EngineError::new(ErrorCode::Script, message)
}
