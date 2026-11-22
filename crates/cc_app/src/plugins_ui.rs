//! Plugins in the app: loading the plugins of the plugins folder into the
//! session (their commands join the engine's catalog, the palette and the
//! ribbon's *Plugin Commands* menu, their buttons make the ribbon's
//! Plugins tab, see [`unavailable`] for their `when`, and their exporters
//! and importers join the ribbon's Export and Import groups, see
//! [`run_file_entry`]) and the Plugins window,
//! which turns them on and off, grants the `files` and `tools` permissions,
//! chooses the external programs their tools run ([`tools_section`]), edits
//! their settings, removes them, and installs and updates them from the
//! community index.
//!
//! Plugins run sandboxed in `cc_plugins`; installing is `cc_plugins::store` (checked
//! archives), and the only network use is here: the index and the archives
//! it lists, downloaded over HTTPS in background threads with bounded sizes,
//! only when the user opens the *Available* tab or installs.

use crate::dialog::Dialog;
use crate::plugin_form::Fmt;
use crate::state::AppState;
use crate::t;
use crate::table;
use crate::theme::{self, PALETTE};
use cc_core::ViewCategory;
use cc_engine::Extension;
use cc_plugins::contributions::current_values;
use cc_plugins::store::{self, Index, IndexPlugin, IndexVersion};
use cc_plugins::{Limits, Permissions, PluginHost, Setting, SettingKind, Tool, When};
use egui::{Context, RichText, Stroke, Ui};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

/// The whole request gives up after this.
const TIMEOUT: Duration = Duration::from_secs(60);

/// Which list the window shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Installed,
    Available,
}

/// The index as the window knows it.
#[derive(Debug, Default)]
enum IndexState {
    #[default]
    NotRead,
    Reading(Receiver<Result<Index, String>>),
    Read(Index),
    Failed(String),
}

/// A plugin of the plugins folder, as listed.
#[derive(Debug, Clone)]
pub struct Listed {
    pub dir: PathBuf,
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub authors: Vec<String>,
    pub license: String,
    pub homepage: String,
    pub asks_files: bool,
    /// It asks for the `tools` permission.
    pub asks_tools: bool,
    /// It asks for the `network` permission (api 3).
    pub asks_network: bool,
    /// The HTTPS hosts declared in `allowed_hosts`.
    pub allowed_hosts: Vec<String>,
    /// Its external programs, with the paths chosen for them.
    pub tools: Vec<Tool>,
    /// For each tool, a program of its name found on `PATH` (offered, never
    /// chosen by itself).
    pub suggestions: Vec<Option<PathBuf>>,
    /// Its commands, when loaded: `(full name, description)`.
    pub commands: Vec<(String, String)>,
    /// It declares settings (shown in the window, also when turned off).
    pub has_settings: bool,
    /// Why it could not be read or loaded.
    pub error: Option<String>,
    /// It comes from the development folder (developer mode): not removed
    /// or set up from the window.
    pub dev: bool,
}

/// A loaded plugin's ribbon button, with what its tooltip tells.
#[derive(Debug, Clone)]
pub struct RibbonButton {
    pub plugin_id: String,
    /// The plugin's name, in the current language.
    pub plugin: String,
    /// What the command does, in the current language.
    pub description: String,
    pub button: cc_plugins::Button,
}

/// A loaded plugin's exporter or importer, for the ribbon's Export and
/// Import groups.
#[derive(Debug, Clone)]
pub struct FileEntry {
    /// The plugin's name, in the current language (the tooltip).
    pub plugin: String,
    pub action: cc_plugins::FileAction,
}

/// The settings of the plugin picked in the window, being edited.
#[derive(Debug)]
struct SettingsEdit {
    id: String,
    settings: Vec<Setting>,
    /// One per setting, in the same order (the command form's fields).
    fields: Vec<crate::plugin_form::Field>,
}

/// An install in a background thread.
#[derive(Debug)]
struct Job {
    name: String,
    id: String,
    files: bool,
    tools: bool,
    result: Receiver<Result<PathBuf, String>>,
}

/// A program's *Test*: running in a background thread, or what it said.
#[derive(Debug)]
enum ToolTest {
    Running(Receiver<Result<String, String>>),
    Done(Result<String, String>),
}

/// Plugins state of the app (kept when the document changes).
#[derive(Debug, Default)]
pub struct Plugins {
    /// The Plugins window is open.
    pub open: bool,
    pub tab: Tab,
    /// The plugins of the folder, read at the last load.
    pub listed: Vec<Listed>,
    /// The plugin picked in the window (by id).
    pub selected: Option<String>,
    pub search: String,
    index: IndexState,
    /// An install running: the plugin's name and id, whether it gets the
    /// `files` permission, and its result.
    job: Option<Job>,
    /// Installs that may work with files (the user ticked the box).
    allow_files: std::collections::BTreeSet<String>,
    /// Installs that may run external programs (the user ticked the box).
    allow_tools: std::collections::BTreeSet<String>,
    /// The *Test* of each program, by plugin and tool id.
    tests: HashMap<(String, String), ToolTest>,
    /// The last install or removal, for the window.
    pub notice: Option<Result<String, String>>,
    /// The folders the plugins were loaded from (loaded again when the
    /// preferences change): the plugins folder and the development folder.
    loaded_from: Option<(Option<PathBuf>, Option<PathBuf>)>,
    /// The loaded plugins' ribbon buttons, in load order then manifest order.
    pub buttons: Vec<RibbonButton>,
    /// The loaded plugins' Component Library catalog sources (api 3).
    pub catalogs: Vec<cc_plugins::Catalog>,
    /// The loaded plugins' exporters and importers, in load order then
    /// manifest order.
    pub exporters: Vec<FileEntry>,
    pub importers: Vec<FileEntry>,
    /// The titles of the loaded plugins' commands, by full name.
    titles: HashMap<String, String>,
    /// The settings being edited in the window.
    settings: Option<SettingsEdit>,
    /// The Plugin console.
    pub console: crate::plugin_console::Console,
    /// Developer mode's watch of the plugin folders.
    pub dev: crate::plugin_dev::Watcher,
}

impl Plugins {
    /// The title of the plugin command `full_name`, in the language the
    /// plugins were loaded in (they load again when it changes).
    pub fn command_title(&self, full_name: &str) -> Option<&str> {
        self.titles.get(full_name).map(String::as_str)
    }

    /// The plugin commands, by plugin: `(plugin name, [(command, description)])`.
    pub fn commands(&self) -> Vec<(&str, &[(String, String)])> {
        self.listed
            .iter()
            .filter(|p| !p.commands.is_empty())
            .map(|p| (p.name.as_str(), p.commands.as_slice()))
            .collect()
    }
}

/// The folders the plugins load from: the plugins folder and, in developer
/// mode, the development folder.
fn sources(state: &AppState) -> (Option<PathBuf>, Option<PathBuf>) {
    (state.prefs.plugins_dir(), state.prefs.plugin_dev_dir())
}

/// Whether a plugin command runs (it waits for an external program, or a
/// form waits for it): the plugins are not loaded again meanwhile.
pub fn busy(state: &AppState) -> bool {
    state.session.has_job()
        || state.dialogs.plugin_form.as_ref().is_some_and(crate::plugin_form::PluginForm::is_waiting)
}

/// A plugin found in a folder (whose `.tools` holds its programs' paths),
/// as the window lists it.
fn listed_item(
    host: &PluginHost,
    errors: &[cc_plugins::PluginError],
    found: store::Installed,
    folder: &std::path::Path,
    dev: bool,
) -> Listed {
    let store::Installed { dir, manifest } = found;
    let error = errors.iter().find(|e| e.dir == dir).map(|e| e.message.clone());
    match manifest {
        Ok(m) => {
            let loaded = host.plugins().iter().find(|p| p.manifest.id == m.id && p.dir == dir);
            let commands = loaded
                .map(|p| p.commands().map(|c| (c.name.clone(), c.description.clone())).collect())
                .unwrap_or_default();
            // A plugin turned off is not loaded: its own text is
            // looked up in its locales here.
            let (name, description) = match loaded {
                Some(p) => (p.display_name(), p.display_description()),
                None if m.api >= 2 => match cc_plugins::Locales::load(&dir) {
                    Ok(l) => (l.resolve(&m.name), l.resolve(&m.description)),
                    Err(_) => (m.name.clone(), m.description.clone()),
                },
                None => (m.name.clone(), m.description.clone()),
            };
            // Read from the manifest, so a plugin turned off has them too.
            let tools = cc_plugins::plugin::tools_schema(&dir, folder).map(|(_, t)| t).unwrap_or_default();
            let suggestions = tools.iter().map(|t| cc_plugins::tools::find_on_path(&t.executables)).collect();
            Listed {
                asks_files: m.requested().files,
                asks_tools: m.requested().tools,
                asks_network: m.requested().network,
                allowed_hosts: m.allowed_hosts,
                tools,
                suggestions,
                has_settings: !m.settings.is_empty(),
                id: m.id,
                name,
                version: m.version,
                description,
                authors: m.authors,
                license: m.license,
                homepage: m.homepage,
                commands,
                error,
                dir,
                dev,
            }
        }
        Err(message) => Listed {
            id: dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            name: dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            version: String::new(),
            description: String::new(),
            authors: Vec::new(),
            license: String::new(),
            homepage: String::new(),
            asks_files: false,
            asks_tools: false,
            asks_network: false,
            allowed_hosts: Vec::new(),
            tools: Vec::new(),
            suggestions: Vec::new(),
            has_settings: false,
            commands: Vec::new(),
            error: Some(message),
            dir,
            dev,
        },
    }
}

/// The plugins of the development folder `dev`: the folder itself when it
/// holds a manifest, else its subfolders that do.
fn dev_plugins(dev: &std::path::Path) -> Vec<store::Installed> {
    if dev.join(cc_plugins::plugin::MANIFEST).is_file() {
        vec![store::Installed { dir: dev.to_path_buf(), manifest: cc_plugins::Manifest::read(dev) }]
    } else {
        store::installed(dev)
    }
}

/// Load the plugins of the plugins folder (and, in developer mode, of the
/// development folder, whose copy of a plugin wins over the installed one)
/// into the session, as the preferences say (turned off ones are left out
/// and run no code; `files` and `tools` only where the user allowed them,
/// by id), and list them for the window. What they print and their load
/// errors go to the console.
pub fn load(state: &mut AppState) {
    use crate::plugin_console::Kind;
    let mut host = PluginHost::new();
    let mut listed = Vec::new();
    let mut notes: Vec<(String, Kind, String)> = Vec::new();
    let (folder, dev) = sources(state);
    let off = state.prefs.plugins_off.clone();
    let files = state.prefs.plugins_files.clone();
    let tools = state.prefs.plugins_tools.clone();
    let network = state.prefs.plugins_network.clone();
    let grant = |m: &cc_plugins::Manifest| {
        (!off.contains(&m.id)).then(|| Permissions {
            files: files.contains(&m.id),
            tools: tools.contains(&m.id),
            network: network.contains(&m.id),
        })
    };
    let mut errors = Vec::new();
    // The ids of the development folder, also of those that fail to load.
    let mut dev_ids = std::collections::BTreeSet::new();
    if let Some(dev) = &dev {
        errors.extend(host.load_folder(dev, Limits::default(), grant));
        for found in dev_plugins(dev) {
            if let Ok(m) = &found.manifest {
                dev_ids.insert(m.id.clone());
            }
            listed.push(listed_item(&host, &errors, found, dev, true));
        }
    }
    if let Some(folder) = &folder {
        let installed = store::installed(folder);
        let overridden: Vec<String> = installed
            .iter()
            .filter_map(|f| f.manifest.as_ref().ok())
            .filter(|m| dev_ids.contains(&m.id))
            .map(|m| m.id.clone())
            .collect();
        for id in &overridden {
            notes.push((id.clone(), Kind::Info, t!("pcon-dev-wins", "id" => id.clone())));
        }
        errors.extend(
            host.load_folder(folder, Limits::default(), |m| if overridden.contains(&m.id) { None } else { grant(m) }),
        );
        for found in installed {
            if found.manifest.as_ref().is_ok_and(|m| overridden.contains(&m.id)) {
                continue;
            }
            listed.push(listed_item(&host, &errors, found, folder, false));
        }
    }
    for e in &errors {
        let id = e.dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let id = if cc_plugins::plugin::valid_id(&id) { id } else { String::new() };
        log::warn!("plugin not loaded: {e}");
        notes.push((id, Kind::Error, e.message.clone()));
    }
    let printed = host.take_messages();
    // What the ribbon and the forms show, read before the host goes into
    // the session (which keeps it as an engine extension).
    let mut buttons = Vec::new();
    let (mut exporters, mut importers) = (Vec::new(), Vec::new());
    let mut titles = HashMap::new();
    for plugin in host.plugins() {
        let descriptions: HashMap<String, String> = plugin.commands().map(|c| (c.name, c.description)).collect();
        for name in descriptions.keys() {
            if let Some(title) = host.command_title(name) {
                titles.insert(name.clone(), title);
            }
        }
        let name = plugin.display_name();
        let entry = |action| FileEntry { plugin: name.clone(), action };
        exporters.extend(plugin.exporters().into_iter().map(entry));
        importers.extend(plugin.importers().into_iter().map(entry));
        for button in plugin.buttons() {
            buttons.push(RibbonButton {
                plugin_id: plugin.manifest.id.clone(),
                plugin: name.clone(),
                description: descriptions.get(&button.command).cloned().unwrap_or_default(),
                button,
            });
        }
    }
    if let Some(cache) = crate::recovery::data_dir().map(|d| d.join("plugin-cache")) {
        host.set_cache_dir(&cache);
    }
    let catalogs = host.catalogs();
    let count = host.plugins().len();
    state.plugins.console.push("", Kind::Info, t!("pcon-loaded", "count" => count));
    for (plugin, kind, text) in notes {
        state.plugins.console.push(&plugin, kind, text);
    }
    crate::plugin_console::record(state, &printed);
    state.session.set_extension(Some(Box::new(host)));
    state.plugins.listed = listed;
    state.plugins.buttons = buttons;
    state.plugins.catalogs = catalogs;
    state.plugins.exporters = exporters;
    state.plugins.importers = importers;
    state.plugins.titles = titles;
    state.plugins.settings = None;
    state.plugins.loaded_from = Some(sources(state));
}

/// Why a plugin button whose `when` is `when` is unavailable now (`None`:
/// it is available); the first term that does not hold tells.
pub fn unavailable(state: &AppState, when: &[When]) -> Option<String> {
    let project = state.project();
    let categories = || state.selection.iter().filter_map(|id| project.element(*id)).map(|e| e.category());
    for term in when {
        let holds = match term {
            When::Selection => !state.selection.is_empty(),
            When::SelectionOne => state.selection.len() == 1,
            When::SelectionOf(name) => {
                !state.selection.is_empty()
                    && state.selection.len() == categories().count()
                    && categories().all(|c| cc_engine::category_name(c) == *name)
            }
            When::View(kind) => {
                state.active_view.and_then(|v| project.view(v)).is_some_and(|v| view_kind(v.category()) == kind)
            }
        };
        if holds {
            continue;
        }
        return Some(match term {
            When::Selection => t!("plug-when-selection"),
            When::SelectionOne => t!("plug-when-one"),
            When::SelectionOf(name) => {
                let category = crate::plugin_form::parse_category(name)
                    .map(crate::actions::t_category)
                    .unwrap_or_else(|| name.clone());
                t!("plug-when-category", "category" => category)
            }
            When::View(kind) => {
                let label = ViewCategory::ALL
                    .into_iter()
                    .find(|c| view_kind(*c) == kind)
                    .map(|c| crate::i18n::tr(c.key()))
                    .unwrap_or_else(|| kind.clone());
                t!("plug-when-view", "kind" => label)
            }
        });
    }
    None
}

/// A plugin's exporter (`save`) or importer was clicked: the save or open
/// dialog for its extensions first, then its command's form with the path
/// chosen (or the command at once, when the path is its only parameter).
pub fn run_file_entry(state: &mut AppState, entry: &FileEntry, save: bool) {
    let action = &entry.action;
    let start = match (save, action.extensions.first()) {
        (true, Some(ext)) => format!("{}.{ext}", state.project().info.name),
        _ => String::new(),
    };
    let Some(path) = crate::plugin_form::choose_path(save, &action.extensions, Some(&action.label), &start) else {
        return;
    };
    open_file_entry(state, entry, &path);
}

/// The form of an exporter's or importer's command with `path` set.
pub fn open_file_entry(state: &mut AppState, entry: &FileEntry, path: &std::path::Path) {
    let mut prefill = Map::new();
    prefill.insert(entry.action.param.clone(), json!(path.to_string_lossy()));
    crate::plugin_form::open_prefilled(state, &entry.action.command, prefill);
}

/// The name a `view:<kind>` term gives a view category.
fn view_kind(category: ViewCategory) -> &'static str {
    match category {
        ViewCategory::FloorPlan => "floor_plan",
        ViewCategory::CeilingPlan => "ceiling_plan",
        ViewCategory::ThreeD => "three_d",
        ViewCategory::Elevation => "elevation",
        ViewCategory::Section => "section",
        ViewCategory::Legend => "legend",
        ViewCategory::Drafting => "drafting",
        ViewCategory::Schedule => "schedule",
        ViewCategory::Diagram => "diagram",
        ViewCategory::Sheet => "sheet",
    }
}

/// Every frame: load the plugins again when their folders changed (the
/// preferences; not while a plugin command runs), and show what plugins
/// printed while their commands ran (logged, in the console, the last one
/// in the status bar).
pub fn drain_messages(state: &mut AppState) {
    if state.plugins.loaded_from != Some(sources(state)) && !busy(state) {
        load(state);
    }
    let lines = state.session.take_extension_messages();
    crate::plugin_console::record(state, &lines);
    if let Some(last) = lines.into_iter().last() {
        state.message = last;
    }
}

/// Load the plugins again (the Plugins window's Reload): the permissions
/// stay, as they are kept by id. Not while a plugin command runs.
pub fn reload(state: &mut AppState) {
    if busy(state) {
        state.message = t!("plug-reload-busy");
        return;
    }
    load(state);
    state.plugins.notice = None;
    state.message = t!("plug-reloaded");
}

/// Open the Plugins window.
pub fn open(state: &mut AppState) {
    state.plugins.open = true;
    state.plugins.notice = None;
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        // No request, and no redirect, leaves https.
        .https_only(true)
        .timeout_global(Some(TIMEOUT))
        .user_agent(format!("ConstructCraft/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// GET an https `url`, at most `limit` bytes.
pub(crate) fn get(url: &str, limit: usize) -> Result<Vec<u8>, String> {
    if !url.starts_with("https://") {
        return Err(format!("not an https link: {url}"));
    }
    // Tests never reach the network.
    if cfg!(test) || crate::update::disabled_by_env() {
        return Err("offline".into());
    }
    let mut response = agent().get(url).call().map_err(|e| e.to_string())?;
    response.body_mut().with_config().limit(limit as u64).read_to_vec().map_err(|e| e.to_string())
}

/// Run `work` in a background thread; the receiver gets its result and
/// `ctx` repaints then.
pub(crate) fn spawn<T: Send + 'static>(
    ctx: &Context,
    name: &str,
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Receiver<Result<T, String>> {
    let (tx, rx) = std::sync::mpsc::channel();
    let ctx = ctx.clone();
    let started = std::thread::Builder::new().name(name.into()).spawn(move || {
        let _ = tx.send(work());
        ctx.request_repaint();
    });
    if let Err(e) = started {
        log::warn!("could not start {name}: {e}");
    }
    rx
}

fn read_index(ctx: &Context) -> Receiver<Result<Index, String>> {
    spawn(ctx, "plugin-index", || Index::parse(&get(store::INDEX_URL, store::MAX_INDEX)?))
}

fn start_install(
    ctx: &Context,
    plugin: IndexPlugin,
    version: IndexVersion,
    folder: PathBuf,
) -> Receiver<Result<PathBuf, String>> {
    spawn(ctx, "plugin-install", move || {
        let archive = get(&version.url, store::MAX_ARCHIVE)?;
        store::install(&archive, &plugin, &version, &folder)
    })
}

/// This build's version, for the index's `min_app`.
fn app_version() -> (u64, u64, u64) {
    store::parse_version(env!("CARGO_PKG_VERSION")).unwrap_or((0, 0, 0))
}

/// Check the background work: the index read, an install finished.
fn poll(state: &mut AppState) {
    let p = &mut state.plugins;
    for test in p.tests.values_mut() {
        if let ToolTest::Running(rx) = test {
            match rx.try_recv() {
                Ok(result) => *test = ToolTest::Done(result),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => *test = ToolTest::Done(Err(String::new())),
            }
        }
    }
    if let IndexState::Reading(rx) = &p.index {
        p.index = match rx.try_recv() {
            Ok(Ok(index)) => IndexState::Read(index),
            Ok(Err(message)) => IndexState::Failed(message),
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => IndexState::Failed(String::new()),
        };
    }
    let Some(job) = &p.job else { return };
    let result = match job.result.try_recv() {
        Ok(r) => r,
        Err(TryRecvError::Empty) => return,
        Err(TryRecvError::Disconnected) => Err(String::new()),
    };
    let Some(Job { name, id, files, tools, .. }) = p.job.take() else { return };
    match result {
        Ok(_) => {
            if files {
                state.prefs.plugins_files.insert(id.clone());
            }
            if tools {
                state.prefs.plugins_tools.insert(id);
            }
            load(state);
            let version = state.plugins.listed.iter().find(|l| l.name == name).map(|l| l.version.clone());
            let version = version.unwrap_or_default();
            state.plugins.notice = Some(Ok(t!("plug-installed-ok", "name" => name, "version" => version)));
        }
        Err(message) => {
            state.plugins.notice = Some(Err(t!("plug-install-failed", "name" => name, "message" => message)));
        }
    }
}

pub fn show(ctx: &Context, state: &mut AppState) {
    if !state.plugins.open {
        return;
    }
    poll(state);
    let (mut reload_button, mut console_button) = (0, 0);
    let busy = busy(state);
    let response = Dialog::new("plugins", t!("dlg-plugins")).width(860.0).show(
        ctx,
        |ui| body(ui, ctx, state),
        |f| {
            reload_button = f.extra(t!("plug-reload"));
            if busy {
                f.disable(reload_button);
                f.tip(reload_button, t!("plug-reload-busy"));
            }
            console_button = f.extra(t!("action-plugin-console"));
            f.secondary(t!("action-close"));
        },
    );
    if response.dismissed {
        state.plugins.open = false;
        return;
    }
    match response.clicked {
        Some(i) if i == reload_button => reload(state),
        // The console is not modal: the Plugins window closes so it can be used.
        Some(i) if i == console_button => {
            state.plugins.console.open = true;
            state.plugins.open = false;
        }
        Some(_) => state.plugins.open = false,
        None => {}
    }
}

fn body(ui: &mut Ui, ctx: &Context, state: &mut AppState) {
    ui.horizontal(|ui| {
        let count = state.plugins.listed.len();
        let installed = t!("plug-tab-installed", "count" => count);
        if ui.selectable_label(state.plugins.tab == Tab::Installed, installed).clicked() {
            state.plugins.tab = Tab::Installed;
            state.plugins.selected = None;
        }
        if ui.selectable_label(state.plugins.tab == Tab::Available, t!("plug-tab-available")).clicked() {
            state.plugins.tab = Tab::Available;
            state.plugins.selected = None;
            if matches!(state.plugins.index, IndexState::NotRead) {
                state.plugins.index = IndexState::Reading(read_index(ctx));
            }
        }
        ui.add_space(12.0);
        ui.add(egui::TextEdit::singleline(&mut state.plugins.search).hint_text(t!("plug-search")).desired_width(220.0));
    });
    match state.prefs.plugins_dir() {
        Some(folder) => ui.label(theme::muted(t!("plug-folder", "path" => folder.display().to_string())).size(11.5)),
        None => ui.label(RichText::new(t!("plug-no-folder")).color(PALETTE.warning)),
    };
    match &state.plugins.notice {
        Some(Ok(text)) => {
            ui.label(RichText::new(text).color(PALETTE.success));
        }
        Some(Err(text)) => {
            ui.label(RichText::new(text).color(PALETTE.danger));
        }
        None => {}
    }
    ui.add_space(6.0);
    match state.plugins.tab {
        Tab::Installed => installed_tab(ui, ctx, state),
        Tab::Available => available_tab(ui, ctx, state),
    }
    ui.add_space(6.0);
    ui.label(theme::muted(t!("plug-review")).size(11.5));
}

fn matches_search(search: &str, texts: &[&str]) -> bool {
    let q = search.trim().to_lowercase();
    q.is_empty() || texts.iter().any(|t| t.to_lowercase().contains(&q))
}

/// The left column: the names, one selectable row each.
fn name_list(ui: &mut Ui, rows: &[(String, String, bool)], selected: &mut Option<String>, empty: &str) {
    egui::Frame::new().stroke(Stroke::new(1.0, PALETTE.border)).show(ui, |ui| {
        egui::ScrollArea::vertical().id_salt("plugin-list").max_height(340.0).auto_shrink([false, false]).show(
            ui,
            |ui| {
                if rows.is_empty() {
                    ui.label(theme::muted(empty));
                }
                for (id, label, faded) in rows {
                    let is = selected.as_deref() == Some(id);
                    let text =
                        if *faded { RichText::new(label).color(PALETTE.text_muted) } else { RichText::new(label) };
                    if ui.selectable_label(is, text).clicked() {
                        *selected = Some(id.clone());
                    }
                }
            },
        );
    });
}

fn installed_tab(ui: &mut Ui, ctx: &Context, state: &mut AppState) {
    let rows: Vec<(String, String, bool)> = state
        .plugins
        .listed
        .iter()
        .filter(|l| matches_search(&state.plugins.search, &[&l.name, &l.id, &l.description]))
        .map(|l| {
            let off = state.prefs.plugins_off.contains(&l.id);
            (l.id.clone(), format!("{}  ·  {}", l.name, l.version), off || l.error.is_some())
        })
        .collect();
    let mut selected = state.plugins.selected.clone();
    let mut changed = false;
    let mut remove = None;
    let mut settings_action = None;
    let mut tool_action = None;
    crate::dialog::two_columns(ui, [300.0, 540.0], |ui, column| {
        if column == 0 {
            name_list(ui, &rows, &mut selected, &t!("plug-none-installed"));
            return;
        }
        let Some(item) = selected.as_ref().and_then(|id| state.plugins.listed.iter().find(|l| &l.id == id)).cloned()
        else {
            ui.label(theme::muted(t!("plug-pick")));
            return;
        };
        header(ui, &item.name, &item.version, &item.authors, &item.license, &item.homepage, &item.description);
        if let Some(error) = &item.error {
            ui.label(RichText::new(t!("plug-error", "message" => error)).color(PALETTE.danger));
        }
        if item.dev {
            let text = t!("plug-dev-copy", "path" => item.dir.display().to_string());
            ui.label(RichText::new(text).size(11.5).color(PALETTE.accent));
        }
        ui.add_space(4.0);
        let mut on = !state.prefs.plugins_off.contains(&item.id);
        if ui.checkbox(&mut on, t!("plug-enabled")).changed() {
            if on {
                state.prefs.plugins_off.remove(&item.id);
            } else {
                state.prefs.plugins_off.insert(item.id.clone());
            }
            changed = true;
        }
        if item.asks_files {
            let mut files = state.prefs.plugins_files.contains(&item.id);
            if ui.checkbox(&mut files, t!("plug-files")).changed() {
                if files {
                    state.prefs.plugins_files.insert(item.id.clone());
                } else {
                    state.prefs.plugins_files.remove(&item.id);
                }
                changed = true;
            }
            ui.label(theme::muted(t!("plug-files-hint")).size(11.5));
        }
        if item.asks_tools {
            let mut tools = state.prefs.plugins_tools.contains(&item.id);
            if ui.checkbox(&mut tools, t!("plug-tools")).changed() {
                if tools {
                    state.prefs.plugins_tools.insert(item.id.clone());
                } else {
                    state.prefs.plugins_tools.remove(&item.id);
                }
                changed = true;
            }
            ui.label(RichText::new(t!("plug-tools-hint")).size(11.5).color(PALETTE.warning));
        }
        if item.asks_network {
            let mut network = state.prefs.plugins_network.contains(&item.id);
            if ui.checkbox(&mut network, t!("plug-network")).changed() {
                if network {
                    state.prefs.plugins_network.insert(item.id.clone());
                } else {
                    state.prefs.plugins_network.remove(&item.id);
                }
                changed = true;
            }
            let hosts = item.allowed_hosts.join(", ");
            ui.label(theme::muted(t!("plug-network-hint", "hosts" => hosts)).size(11.5));
        }
        ui.add_space(6.0);
        ui.label(RichText::new(t!("plug-commands")).strong());
        if item.commands.is_empty() {
            ui.label(theme::muted(t!("plug-no-commands")));
        }
        egui::ScrollArea::vertical().id_salt("plugin-commands").max_height(150.0).show(ui, |ui| {
            for (name, description) in &item.commands {
                ui.label(RichText::new(name).monospace().size(12.0));
                if !description.is_empty() {
                    ui.label(theme::muted(description).size(11.5));
                }
            }
        });
        // A development copy is set up and removed in its own folder.
        if item.dev {
            return;
        }
        tool_action = tools_section(ui, ctx, state, &item);
        settings_action = settings_section(ui, state, &item);
        ui.add_space(6.0);
        if crate::dialog::styled_button(ui, &t!("plug-remove"), crate::dialog::Style::Danger).clicked() {
            remove = Some(item.clone());
        }
    });
    state.plugins.selected = selected;
    if changed {
        load(state);
    }
    match settings_action {
        Some(SettingsAction::Save) => save_settings(state),
        Some(SettingsAction::Reset) => write_settings(state, Map::new()),
        None => {}
    }
    if let Some((id, tool, path)) = tool_action {
        choose_program(state, &id, &tool, &path);
    }
    if let Some(item) = remove {
        let folder = item.dir.parent().map(PathBuf::from).unwrap_or_default();
        state.plugins.notice = Some(match store::uninstall(&folder, &item.id) {
            Ok(()) => {
                state.prefs.plugins_off.remove(&item.id);
                state.prefs.plugins_files.remove(&item.id);
                state.prefs.plugins_tools.remove(&item.id);
                state.prefs.plugins_network.remove(&item.id);
                state.plugins.selected = None;
                Ok(t!("plug-removed", "name" => item.name.clone()))
            }
            Err(message) => Err(t!("plug-remove-failed", "name" => item.name.clone(), "message" => message)),
        });
        load(state);
    }
}

/// The *External programs* section of the selected plugin: for each tool its
/// name, the program chosen (and why it cannot run), *Browse…*, the program
/// of its name found on `PATH` (*Use …*), *Test* (its version check, in a
/// background thread) with what it answered, and where to get it. Returns
/// the program chosen: plugin id, tool id, path.
fn tools_section(ui: &mut Ui, ctx: &Context, state: &mut AppState, item: &Listed) -> Option<(String, String, PathBuf)> {
    if item.tools.is_empty() {
        return None;
    }
    let mut chosen = None;
    ui.add_space(6.0);
    table::section(ui, &format!("plug-tools-{}", item.id), t!("plug-programs"), |ui| {
        for (i, tool) in item.tools.iter().enumerate() {
            egui::Frame::new().inner_margin(egui::Margin::symmetric(8, 6)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(&tool.name).strong());
                let shown = match &tool.path {
                    Some(path) => RichText::new(path.display().to_string()).monospace().size(11.5),
                    None => theme::muted(t!("plug-program-none")).size(11.5),
                };
                ui.add(egui::Label::new(shown).truncate());
                if let (Some(_), Some(problem)) = (&tool.path, tool.problem()) {
                    let text = t!("plug-program-problem", "message" => problem);
                    ui.label(RichText::new(text).size(11.5).color(PALETTE.warning));
                }
                let key = (item.id.clone(), tool.id.clone());
                ui.horizontal_wrapped(|ui| {
                    if ui.button(t!("pform-browse")).clicked()
                        && let Some(path) = rfd::FileDialog::new().set_title(tool.name.as_str()).pick_file()
                    {
                        chosen = Some((item.id.clone(), tool.id.clone(), path));
                    }
                    let suggestion =
                        item.suggestions.get(i).cloned().flatten().filter(|s| tool.path.as_ref() != Some(s));
                    if let Some(found) = suggestion {
                        let label = t!("plug-program-use", "path" => found.display().to_string());
                        if ui.button(label).clicked() {
                            chosen = Some((item.id.clone(), tool.id.clone(), found));
                        }
                    }
                    let testing = matches!(state.plugins.tests.get(&key), Some(ToolTest::Running(_)));
                    let can_test = tool.problem().is_none() && !testing;
                    if ui.add_enabled(can_test, egui::Button::new(t!("plug-program-test"))).clicked()
                        && let Some(path) = tool.path.clone()
                    {
                        let (plugin, tool) = (item.id.clone(), tool.clone());
                        let rx = spawn(ctx, "plugin-tool-test", move || cc_plugins::tools::test(&plugin, &tool, &path));
                        state.plugins.tests.insert(key.clone(), ToolTest::Running(rx));
                    }
                    if tool.homepage.starts_with("https://") {
                        ui.hyperlink_to(t!("plug-program-homepage"), &tool.homepage);
                    }
                });
                match state.plugins.tests.get(&key) {
                    Some(ToolTest::Running(_)) => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(theme::muted(t!("plug-program-testing")).size(11.5));
                        });
                    }
                    Some(ToolTest::Done(Ok(answer))) => {
                        let text = if answer.is_empty() { t!("plug-program-test-ok") } else { answer.clone() };
                        ui.add(egui::Label::new(RichText::new(text).monospace().size(11.5).color(PALETTE.text_muted)));
                    }
                    Some(ToolTest::Done(Err(message))) => {
                        let text = t!("plug-program-test-failed", "message" => message);
                        ui.label(RichText::new(text).size(11.5).color(PALETTE.danger));
                    }
                    None => {}
                }
            });
        }
    });
    chosen
}

/// Store `path` as the program of the tool `tool` of the plugin `id` (when
/// it has the tool's name) and load the plugins again, so it runs it.
fn choose_program(state: &mut AppState, id: &str, tool: &str, path: &std::path::Path) {
    let Some(folder) = state.prefs.plugins_dir() else { return };
    let Some(item) = state.plugins.listed.iter().find(|l| l.id == id) else { return };
    let Some(spec) = item.tools.iter().find(|t| t.id == tool) else { return };
    let name = spec.name.clone();
    let checked = cc_plugins::tools::check_path(&spec.executables, path).and_then(|()| {
        let mut paths = cc_plugins::tools::read_tool_paths(&folder, id);
        paths.insert(tool.to_string(), path.to_path_buf());
        cc_plugins::tools::write_tool_paths(&folder, id, &paths)
    });
    state.plugins.tests.remove(&(id.to_string(), tool.to_string()));
    state.plugins.notice = Some(match checked {
        Ok(()) => Ok(t!("plug-program-saved", "tool" => name, "path" => path.display().to_string())),
        Err(message) => Err(t!("plug-program-problem", "message" => message)),
    });
    load(state);
}

/// What the buttons under a plugin's settings ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsAction {
    Save,
    Reset,
}

/// The JSON Schema property the command form makes a setting's field from.
fn setting_property(s: &Setting) -> Value {
    let kind = match s.kind {
        SettingKind::Boolean => "boolean",
        SettingKind::Number => "number",
        SettingKind::Integer => "integer",
        SettingKind::String => "string",
    };
    let mut prop = json!({"type": kind, "title": s.title, "description": s.description, "default": s.default});
    if let Some(ui) = &s.ui {
        prop["ui"] = json!(ui);
    }
    if let Some(min) = s.minimum {
        prop["minimum"] = json!(min);
    }
    if let Some(max) = s.maximum {
        prop["maximum"] = json!(max);
    }
    if !s.enum_values.is_empty() {
        prop["enum"] = json!(s.enum_values);
        prop["enum_labels"] = json!(s.enum_labels);
    }
    prop
}

/// The settings of the plugin `item` with their stored values, read from
/// its manifest (its code does not run, so one turned off has them too).
fn settings_edit(state: &AppState, item: &Listed, fmt: &Fmt) -> Option<SettingsEdit> {
    let folder = state.prefs.plugins_dir()?;
    let (_, settings) = cc_plugins::plugin::settings_schema(&item.dir).ok()?;
    let values = current_values(&settings, &store::read_settings(&folder, &item.id));
    let properties: Map<String, Value> = settings.iter().map(|s| (s.key.clone(), setting_property(s))).collect();
    let keys: Vec<&str> = settings.iter().map(|s| s.key.as_str()).collect();
    let mut fields = crate::plugin_form::fields(&json!({"properties": properties, "required": keys}), fmt);
    for field in &mut fields {
        if let Some(value) = values.get(&field.name) {
            field.set_value(value.clone(), fmt);
        }
    }
    Some(SettingsEdit { id: item.id.clone(), settings, fields })
}

/// The Settings section of the selected plugin, with Save and Reset to
/// defaults under it; returns the button clicked.
fn settings_section(ui: &mut Ui, state: &mut AppState, item: &Listed) -> Option<SettingsAction> {
    if !item.has_settings {
        return None;
    }
    let fmt = Fmt::current();
    if state.plugins.settings.as_ref().is_none_or(|e| e.id != item.id) {
        state.plugins.settings = settings_edit(state, item, &fmt);
    }
    let off = state.prefs.plugins_off.contains(&item.id);
    let edit = state.plugins.settings.as_mut()?;
    ui.add_space(6.0);
    table::section(ui, &format!("plug-settings-{}", item.id), t!("plug-settings"), |ui| {
        for field in &mut edit.fields {
            let label = field.label.clone();
            let tip = (!field.description.is_empty()).then(|| field.description.clone());
            table::row_tip(ui, &label, tip.as_deref(), |ui| crate::plugin_form::plain_cell(ui, field, &fmt));
        }
    });
    if off {
        ui.label(theme::muted(t!("plug-settings-off")).size(11.5));
    }
    let mut action = None;
    ui.horizontal(|ui| {
        if ui.button(t!("plug-settings-save")).clicked() {
            action = Some(SettingsAction::Save);
        }
        if ui.button(t!("plug-settings-reset")).clicked() {
            action = Some(SettingsAction::Reset);
        }
    });
    action
}

/// Store the settings being edited: the valid values that differ from the
/// defaults (a value typed wrong keeps the one stored before).
fn save_settings(state: &mut AppState) {
    let Some(edit) = &state.plugins.settings else { return };
    let Some(folder) = state.prefs.plugins_dir() else { return };
    let stored = store::read_settings(&folder, &edit.id);
    let mut values = Map::new();
    for (setting, field) in edit.settings.iter().zip(&edit.fields) {
        let typed = match field.problem() {
            None => field.effective(),
            Some(_) => stored.get(&setting.key),
        };
        if let Some(value) = typed.and_then(|v| setting.validate(v).ok())
            && value != setting.default
        {
            values.insert(setting.key.clone(), value);
        }
    }
    write_settings(state, values);
}

/// Write the settings of the plugin being edited and load the plugins again,
/// so the plugin runs with them.
fn write_settings(state: &mut AppState, values: Map<String, Value>) {
    let Some(edit) = &state.plugins.settings else { return };
    let Some(folder) = state.prefs.plugins_dir() else { return };
    let id = edit.id.clone();
    let name = state.plugins.listed.iter().find(|l| l.id == id).map(|l| l.name.clone()).unwrap_or(id.clone());
    state.plugins.notice = Some(match store::write_settings(&folder, &id, &values) {
        Ok(()) => Ok(t!("plug-settings-saved", "name" => name)),
        Err(message) => Err(t!("plug-settings-failed", "name" => name, "message" => message)),
    });
    load(state);
}

/// Name, version, authors, license, page and description.
fn header(
    ui: &mut Ui,
    name: &str,
    version: &str,
    authors: &[String],
    license: &str,
    homepage: &str,
    description: &str,
) {
    ui.label(theme::title(name));
    let mut facts = Vec::new();
    if !version.is_empty() {
        facts.push(t!("plug-version", "version" => version));
    }
    if !authors.is_empty() {
        facts.push(t!("plug-by", "authors" => authors.join(", ")));
    }
    if !license.is_empty() {
        facts.push(t!("plug-license", "license" => license));
    }
    ui.label(theme::muted(facts.join("  ·  ")).size(11.5));
    if homepage.starts_with("https://") {
        ui.hyperlink_to(t!("plug-homepage"), homepage);
    }
    if !description.is_empty() {
        ui.add_space(4.0);
        ui.label(description);
    }
}

fn available_tab(ui: &mut Ui, ctx: &Context, state: &mut AppState) {
    let index = match &state.plugins.index {
        IndexState::NotRead | IndexState::Reading(_) => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(t!("plug-loading"));
            });
            return;
        }
        IndexState::Failed(message) => {
            ui.label(RichText::new(t!("plug-index-failed", "message" => message)).color(PALETTE.danger));
            if ui.button(t!("plug-refresh")).clicked() {
                state.plugins.index = IndexState::Reading(read_index(ctx));
            }
            return;
        }
        IndexState::Read(index) => index.clone(),
    };
    if ui.button(t!("plug-refresh")).clicked() {
        state.plugins.index = IndexState::Reading(read_index(ctx));
    }
    let rows: Vec<(String, String, bool)> = index
        .plugins
        .iter()
        .filter(|p| matches_search(&state.plugins.search, &[&p.name, &p.id, &p.description, &p.tags.join(" ")]))
        .map(|p| {
            let best = p.best(app_version());
            let shown = best.or_else(|| p.newest());
            let label = format!("{}  ·  {}", p.name, shown.map_or("", |v| v.version.as_str()));
            (p.id.clone(), label, best.is_none())
        })
        .collect();
    let mut selected = state.plugins.selected.clone();
    let mut install = None;
    crate::dialog::two_columns(ui, [300.0, 540.0], |ui, column| {
        if column == 0 {
            name_list(ui, &rows, &mut selected, &t!("plug-index-empty"));
            return;
        }
        let Some(plugin) = selected.as_ref().and_then(|id| index.plugins.iter().find(|p| &p.id == id)) else {
            ui.label(theme::muted(t!("plug-pick")));
            return;
        };
        let best = plugin.best(app_version());
        // An incompatible plugin shows its newest version and what it needs.
        let shown = best.or_else(|| plugin.newest());
        let version = shown.map_or_else(String::new, |v| v.version.clone());
        let homepage = plugin.homepage.clone().or_else(|| plugin.repository.clone()).unwrap_or_default();
        header(ui, &plugin.name, &version, &plugin.authors, &plugin.license, &homepage, &plugin.description);
        if let Some(notes) = shown.and_then(|v| v.notes.as_ref()) {
            ui.label(theme::muted(notes).size(11.5));
        }
        ui.add_space(6.0);
        let Some(best) = best else {
            let current = env!("CARGO_PKG_VERSION");
            let why = match shown.and_then(|v| v.min_app.clone()) {
                Some(min) if shown.is_some_and(|v| v.api <= cc_plugins::API_VERSION) => {
                    t!("plug-needs-app", "version" => min, "current" => current)
                }
                _ => t!("plug-incompatible"),
            };
            ui.label(RichText::new(why).color(PALETTE.warning));
            return;
        };
        let asks_files = best.permissions.iter().any(|p| p == "files");
        if asks_files {
            let mut allow = state.plugins.allow_files.contains(&plugin.id);
            if ui.checkbox(&mut allow, t!("plug-files")).changed() {
                if allow {
                    state.plugins.allow_files.insert(plugin.id.clone());
                } else {
                    state.plugins.allow_files.remove(&plugin.id);
                }
            }
            ui.label(theme::muted(t!("plug-files-hint")).size(11.5));
        }
        let asks_tools = best.permissions.iter().any(|p| p == "tools");
        if asks_tools {
            let mut allow = state.plugins.allow_tools.contains(&plugin.id);
            if ui.checkbox(&mut allow, t!("plug-tools")).changed() {
                if allow {
                    state.plugins.allow_tools.insert(plugin.id.clone());
                } else {
                    state.plugins.allow_tools.remove(&plugin.id);
                }
            }
            ui.label(RichText::new(t!("plug-tools-hint")).size(11.5).color(PALETTE.warning));
        }
        let installed = state.plugins.listed.iter().find(|l| l.id == plugin.id);
        let newer = installed.is_none_or(|l| {
            store::parse_version(&l.version)
                .is_none_or(|have| store::parse_version(&best.version).is_some_and(|v| v > have))
        });
        if let Some(job) = &state.plugins.job {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(t!("plug-installing", "name" => job.name.clone()));
            });
        } else if !newer {
            ui.label(theme::muted(
                t!("plug-installed-version", "version" => installed.map(|l| l.version.clone()).unwrap_or_default()),
            ));
        } else {
            let label = if installed.is_some() {
                t!("plug-update", "version" => best.version.clone())
            } else {
                t!("plug-install")
            };
            if crate::dialog::styled_button(ui, &label, crate::dialog::Style::Primary).clicked() {
                install = Some((plugin.clone(), best.clone(), asks_files, asks_tools));
            }
        }
    });
    state.plugins.selected = selected;
    if let Some((plugin, version, asks_files, asks_tools)) = install {
        let Some(folder) = state.prefs.plugins_dir() else { return };
        let files = asks_files && state.plugins.allow_files.contains(&plugin.id);
        let tools = asks_tools && state.plugins.allow_tools.contains(&plugin.id);
        state.plugins.notice = None;
        let (name, id) = (plugin.name.clone(), plugin.id.clone());
        let result = start_install(ctx, plugin, version, folder);
        state.plugins.job = Some(Job { name, id, files, tools, result });
    }
}

/// Show `index` in the Available tab as if it had been read.
#[cfg(test)]
pub fn set_index(state: &mut AppState, index: Index) {
    state.plugins.index = IndexState::Read(index);
    state.plugins.tab = Tab::Available;
}

/// Choose `path` for the tool `tool` of the plugin `id`, as Browse… does
/// (tests).
#[cfg(test)]
pub fn choose_program_for_test(state: &mut AppState, id: &str, tool: &str, path: &std::path::Path) {
    choose_program(state, id, tool, path);
}

/// Wait for the *Test* of every program to end (tests).
#[cfg(test)]
pub fn finish_tests(state: &mut AppState) {
    for test in state.plugins.tests.values_mut() {
        if let ToolTest::Running(rx) = test {
            *test = ToolTest::Done(rx.recv_timeout(Duration::from_secs(20)).unwrap_or_else(|e| Err(e.to_string())));
        }
    }
}

/// What the *Test* of the tool `tool` of the plugin `id` answered (tests).
#[cfg(test)]
pub fn test_answer(state: &AppState, id: &str, tool: &str) -> Option<Result<String, String>> {
    match state.plugins.tests.get(&(id.to_string(), tool.to_string()))? {
        ToolTest::Done(result) => Some(result.clone()),
        ToolTest::Running(_) => None,
    }
}

/// Wait for the install running (tests: it fails at once, offline).
#[cfg(test)]
pub fn finish_job(state: &mut AppState) {
    if let Some(job) = &state.plugins.job
        && let Ok(result) = job.result.recv_timeout(Duration::from_secs(10))
    {
        let Some(Job { name, .. }) = state.plugins.job.take() else { return };
        state.plugins.notice = Some(match result {
            Ok(_) => Ok(name),
            Err(message) => Err(t!("plug-install-failed", "name" => name, "message" => message)),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cc_core::{Category, ElementId};
    use std::collections::BTreeSet;

    fn state() -> AppState {
        let project = crate::home::sample_project("casa_simples").unwrap();
        AppState::new(cc_core::Document::new(project))
    }

    fn first(state: &AppState, category: Category) -> ElementId {
        state.project().elements.values().find(|e| e.category() == category).unwrap().id
    }

    #[test]
    fn when_terms_all_hold_or_the_first_that_does_not_tells_why() {
        let mut s = state();
        let (wall, door) = (first(&s, Category::Wall), first(&s, Category::Door));
        let walls = vec![When::SelectionOf("wall".into())];
        let one = [When::SelectionOne];
        assert_eq!(unavailable(&s, &[]), None, "no terms: always");
        assert_eq!(unavailable(&s, &[When::Selection]), Some(t!("plug-when-selection")));
        assert_eq!(unavailable(&s, &one), Some(t!("plug-when-one")));
        let why_walls = t!("plug-when-category", "category" => crate::actions::t_category(Category::Wall));
        assert_eq!(unavailable(&s, &walls), Some(why_walls.clone()), "nothing selected");
        s.selection = BTreeSet::from([wall]);
        assert_eq!(unavailable(&s, &[When::Selection, When::SelectionOne, walls[0].clone()]), None);
        s.selection.insert(door);
        assert_eq!(unavailable(&s, &[When::Selection]), None);
        assert_eq!(unavailable(&s, &one), Some(t!("plug-when-one")));
        assert_eq!(unavailable(&s, &walls), Some(why_walls), "a door too");
        assert!(unavailable(&s, &[When::SelectionOf("door".into())]).is_some());
        // A selected id that is not in the project does not count as a wall.
        s.selection = BTreeSet::from([wall, ElementId(u64::MAX)]);
        assert!(unavailable(&s, &walls).is_some());
        // The active view's kind.
        let three_d = [When::View("three_d".into())];
        let plan = s.project().views.values().find(|v| v.category() == ViewCategory::FloorPlan).unwrap().id;
        let view3d = s.project().views.values().find(|v| v.category() == ViewCategory::ThreeD).unwrap().id;
        s.active_view = Some(plan);
        let why = t!("plug-when-view", "kind" => crate::i18n::tr(ViewCategory::ThreeD.key()));
        assert_eq!(unavailable(&s, &three_d), Some(why));
        assert_eq!(unavailable(&s, &[When::View("floor_plan".into())]), None);
        s.active_view = Some(view3d);
        assert_eq!(unavailable(&s, &three_d), None);
        s.active_view = None;
        assert!(unavailable(&s, &three_d).is_some(), "no view open");
        // Every view kind a manifest may name has its view category.
        for kind in cc_plugins::contributions::VIEW_KINDS {
            assert!(ViewCategory::ALL.into_iter().any(|c| view_kind(c) == kind), "{kind}");
        }
    }
}
