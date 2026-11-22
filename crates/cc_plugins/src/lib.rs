//! Lua scripts and plugins for ConstructCraft.
//!
//! Lua 5.4 runs sandboxed ([`sandbox`], from `cc_script`): no files, no loading of code, an
//! instruction budget and a memory limit. Lua code reaches the project only
//! through the engine's commands (`ccad.run`, see [`api`]), the same ones the
//! GUI, the command line and the MCP server use.
//!
//! - [`run_script`] runs a script once (`ccad lua file.lua`).
//! - [`plugin`] loads plugins (a folder with `plugin.toml` and `main.lua`)
//!   whose commands join the session's catalog through [`PluginHost`], an
//!   engine [`cc_engine::Extension`]: each plugin command is one undo step
//!   and leaves nothing behind when it fails.
//! - Plugins written for API 2 also declare ribbon buttons and settings in
//!   their manifest ([`contributions`]), translate their text
//!   ([`locales`]), give command parameters UI hints, and keep their own
//!   data in the project (`ccad.data`).
//! - Plugins react to the session's events with `ccad.on` (`project_opened`,
//!   `project_saving`, `project_saved`, `model_changed`): handlers that may
//!   query and keep their own data, run by [`PluginHost`] as the
//!   extension's [`cc_engine::Extension::on_event`].
//! - [`store`] installs plugins from the community index and keeps the
//!   values of their settings.
//! - Plugins with the `tools` permission run external programs the user
//!   chose ([`tools`]) with `ccad.process.run`, on a background thread
//!   ([`process`]); the command waits without holding the session
//!   ([`cc_engine::Outcome::Pending`]).
//! - [`testing`] runs a plugin's own tests (`tests/*_test.lua`, `ccad
//!   plugins test`), each over a fresh session with the plugin loaded.

pub mod api;
pub mod contributions;
pub mod http;
pub mod locales;
pub mod plugin;
pub mod process;
pub mod script;
pub mod store;
pub mod testing;
pub mod tools;

pub use cc_script::{convert, sandbox};

pub use api::{API_VERSION, CommandRun, FileRun, HttpRun, ProcessRun};
pub use contributions::{Button, ButtonSize, Catalog, CatalogSpec, FileAction, Setting, SettingKind, When};
pub use locales::Locales;
pub use plugin::{Manifest, Permissions, Plugin, PluginError, PluginHost};
pub use sandbox::Limits;
pub use script::{ScriptOptions, ScriptOutput, run_script};
pub use tools::{Tool, ToolSpec};
