---@meta ccad
-- Type definitions of the `ccad` table of ConstructCraft's Lua scripts and
-- plugins (plugin API 2), for the Lua language server (LuaLS / sumneko):
-- list the folder holding this file in `workspace.library` of `.luarc.json`.
-- `ccad plugins new` copies it into a new plugin's `types/` folder.
-- The reference is docs/automation.md (Lua scripts, Plugins); a test checks
-- that every name of the runtime table is declared here.

---JSON null (Lua's `nil` leaves a field out).
---@class ccad.Null

---A command's failure, as `ccad.try` returns it.
---@class ccad.Error
---@field code ccad.ErrorCode
---@field message string

---@alias ccad.ErrorCode
---| "unknown_command"
---| "invalid_arguments"
---| "not_found"
---| "rejected"
---| "io"
---| "internal"
---| "script"
---| "pending"
---| "cancelled"

---A command of the catalog, as `ccad.commands` lists it.
---@class ccad.CommandEntry
---@field name string
---@field kind "query"|"edit"|"file"
---@field group string
---@field description string

---A command with the JSON Schema of its parameters, as `ccad.describe` returns it.
---@class ccad.CommandDescription: ccad.CommandEntry
---@field schema table

---The `ccad` table.
---@class ccad
---@field version string The ConstructCraft version.
---@field api integer The plugin API version this build provides (3).
---@field null ccad.Null JSON null.
---@field args table Scripts only: the script's arguments (also `...`).
---@field json ccad.json
---@field plugin ccad.plugin Plugins only: the plugin's id, name and version.
---@field settings table<string, boolean|number|integer|string> Plugins only: the current values of its settings (read-only).
---@field data ccad.data Plugins only: its own data in the project.
---@field file ccad.file Plugins only: the files a command's arguments grant.
---@field process ccad.process Plugins only: run the external programs the user chose.
---@field http ccad.http Plugins only (API 3): scoped HTTPS requests and per-plugin file cache.
---@field test ccad.test Tests only (`ccad plugins test`): assertions.
ccad = {}

---Run an engine command; returns its result, or raises an error when it fails.
---@param name string The command's name (`wall_create`, `quick_rooms__rectangle_room`…).
---@param args? table Its arguments (a JSON object).
---@return any
function ccad.run(name, args) end

---Run an engine command; returns its result, or `nil` and the error.
---@param name string
---@param args? table
---@return any result
---@return ccad.Error|nil err
function ccad.try(name, args) end

---The commands of the catalog (the plugins' commands too).
---@return ccad.CommandEntry[]
function ccad.commands() end

---One command, with the JSON Schema of its parameters; `nil` when there is none.
---@param name string
---@return ccad.CommandDescription|nil
function ccad.describe(name) end

---Mark `t` (or a new table) as a list, so that an empty one is sent as `[]`.
---@generic T: table
---@param t? T
---@return T
function ccad.array(t) end

---Plugins only, while `main.lua` loads: register a command.
---@param spec ccad.CommandSpec
function ccad.command(spec) end

---Plugins only (API 3), while `main.lua` loads: register a Component Library catalog provider.
---@param spec ccad.CatalogSpec
function ccad.catalog(spec) end

---Plugins only, while `main.lua` loads: run `handler` on a session event.
---@overload fun(event: "project_opened", handler: fun(e: ccad.ProjectOpenedEvent))
---@overload fun(event: "project_saving", handler: fun(e: ccad.ProjectSavedEvent))
---@overload fun(event: "project_saved", handler: fun(e: ccad.ProjectSavedEvent))
---@overload fun(event: "model_changed", handler: fun(e: ccad.ModelChangedEvent))
---@param event ccad.EventName
---@param handler fun(e: table)
function ccad.on(event, handler) end

---Scoped HTTPS requests and isolated per-plugin cache (API 3, `network` permission).
---@class ccad.http
ccad.http = {}

---GET an HTTPS URL whose host is in `allowed_hosts`.
---@param url string
---@param options? { max_bytes?: integer, timeout?: integer }
---@return string body
---@return integer status
function ccad.http.get(url, options) end

---Download an HTTPS URL into the plugin's cache under `key` (optionally verifying `sha256`).
---@param url string
---@param key string
---@param options? { sha256?: string, max_bytes?: integer, timeout?: integer }
---@return string path
function ccad.http.download(url, key, options) end

---Return the local path of `key` in the plugin's cache if present (never uses the network).
---@param key string
---@param options? { sha256?: string }
---@return string|nil path
function ccad.http.cached(key, options) end

---@class ccad.CatalogSpec
---@field id string
---@field load fun(): table
---@field search fun(query: table): table
---@field thumbnail? fun(item_id: string): string|nil
---@field prepare fun(item_id: string): table

---JSON text to and from Lua values.
---@class ccad.json
ccad.json = {}

---@param value any
---@return string
function ccad.json.encode(value) end

---@param text string
---@return any
function ccad.json.decode(text) end

---The plugin's manifest, as `ccad.plugin`.
---@class ccad.plugin
---@field id string
---@field name string
---@field version string
ccad.plugin = {}

---A plugin's own data in the project (on elements, or on the project itself).
---@class ccad.data
ccad.data = {}

---What the plugin keeps on `element` (`nil`: on the project), or `ccad.null`.
---@param element integer|nil
---@return any
function ccad.data.get(element) end

---Keep `value` (any JSON, up to 256 KiB) on `element` (`nil`: the project); `nil` removes it. An edit.
---@param element integer|nil
---@param value any
function ccad.data.set(element, value) end

---Where the plugin keeps data.
---@return { project: boolean, elements: integer[] }
function ccad.data.list() end

---The files a plugin command's `open_path` and `save_path` arguments grant (while it runs).
---@class ccad.file
ccad.file = {}

---The bytes of a granted `open_path` file (at most 64 MiB).
---@param path string
---@return string
function ccad.file.read(path) end

---Write a granted `save_path` file when the command succeeds; `data` is a string or a list of strings, joined.
---@param path string
---@param data string|string[]
function ccad.file.write(path, data) end

---External programs (`tools` permission, a program the user chose).
---@class ccad.process
ccad.process = {}

---What to run: all optional.
---@class ccad.ProcessRequest
---@field args? string[] Passed as they are, without a shell.
---@field files? table<string, string> Written into the run folder first (name → content).
---@field copy? table<string, string> Granted `open_path` files copied into the run folder (name → path).
---@field outputs? string[] Names (`*` matches within a name) read back from the run folder.
---@field timeout? number Seconds: default 300, at most 7200.

---How the program ended.
---@class ccad.ProcessResult
---@field code integer|nil The exit code (`nil` when it has none, as after a timeout).
---@field stdout string
---@field stderr string
---@field truncated boolean Standard output or error was cut at 1 MiB.
---@field timed_out boolean
---@field files table<string, string> The outputs found (name → content).

---Run the program chosen for the manifest's tool `tool` and wait for it (only in a command's own `run`).
---@param tool string
---@param request? ccad.ProcessRequest
---@return ccad.ProcessResult
function ccad.process.run(tool, request) end

---Assertions of plugin tests: each raises an error at the caller's line.
---@class ccad.test
ccad.test = {}

---`actual` equals `expected` (tables compared deeply).
---@param actual any
---@param expected any
---@param msg? string
function ccad.test.eq(actual, expected, msg) end

---Two numbers differ by at most `tol` (default 1e-9).
---@param actual number
---@param expected number
---@param tol? number
---@param msg? string
function ccad.test.near(actual, expected, tol, msg) end

---`fn` raises an error whose text holds `pattern` (plain text, or a Lua pattern); returns the error's text.
---@param fn fun()
---@param pattern? string
---@return string
function ccad.test.fails(fn, pattern) end

---`value` is true (not `nil` or `false`); returns it.
---@generic T
---@param value T
---@param msg? string
---@return T
function ccad.test.ok(value, msg) end

---@alias ccad.EventName
---| "project_opened"
---| "project_saving"
---| "project_saved"
---| "model_changed"

---@class ccad.ProjectOpenedEvent
---@field path string|nil The project's file (`nil` when it has none yet).
---@field new boolean It has no file yet.

---`project_saving` and `project_saved`.
---@class ccad.ProjectSavedEvent
---@field path string

---@class ccad.ModelChangedEvent
---@field command string The command that changed the project (`undo`, `redo`, a plugin command…).
---@field added integer[]
---@field modified integer[]
---@field removed integer[]
---@field truncated boolean|nil A list was cut at 10 000 ids.
---@field other boolean|nil Something other than elements changed.

---@alias ccad.ParamUi
---| "length"    # a number: a length in the project's units
---| "element"   # an integer: an element id (`category` narrows it)
---| "elements"  # an array: element ids (`category` narrows them)
---| "point"     # an array: a point [x, y]
---| "level"     # an integer: a level id
---| "view"      # an integer: a view id
---| "type"      # an integer: a type id (`category` narrows it)
---| "material"  # a string: a material name
---| "save_path" # a string: a file to write (granted to the command)
---| "open_path" # a string: a file to read (granted to the command)

---A parameter of a command: a JSON Schema property with UI hints.
---@class ccad.Param
---@field type "number"|"integer"|"string"|"boolean"|"array"|"object"
---@field title? string A short name, may be "%key%".
---@field description? string May be "%key%".
---@field default? any
---@field enum? any[]
---@field enum_labels? string[] One per `enum` value, may be "%key%".
---@field minimum? number
---@field maximum? number
---@field items? table
---@field ui? ccad.ParamUi
---@field category? string With `element`, `elements` or `type`.
---@field extensions? string[] With `save_path` or `open_path`: without the dot.

---The parameters of a command: a JSON Schema object.
---@class ccad.Params
---@field properties? table<string, ccad.Param>
---@field required? string[]

---What `ccad.command` takes.
---@class ccad.CommandSpec
---@field name string Lowercase letters, digits and single underscores; the command is `<plugin id>__<name>`.
---@field title? string May be "%key%".
---@field description? string May be "%key%".
---@field kind? "query"|"edit"|"file" Default "edit"; "file" needs the `files` permission.
---@field params? ccad.Params
---@field run fun(args: table): any
