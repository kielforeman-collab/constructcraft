//! Component library: browse the component files (`.ccomp`) of the folders
//! the user adds (such as a copy of the ConstructCraft Components library),
//! search them, see a component's types, parameters, connectors and plan,
//! load it into the project, and save the project's components into a
//! library folder. Listing, loading and saving are engine commands.
//!
//! The window has a second source, 3D CraftHouse: the open library
//! published online, listed from its catalog; a component picked there is
//! downloaded and then loaded like a file of a folder (see
//! [`crate::crafthouse`]).
//!
//! The window is a browser in three columns: filters on the left (the
//! component kinds, then the folders or the authors), the components as
//! cards with their 3D thumbnails in the middle, and the details of the one
//! picked on the right.

use crate::crafthouse::{FileStatus, Status};
use crate::dialog::Dialog;
use crate::i18n::tr;
use crate::state::AppState;
use crate::t;
use crate::theme::{self, PALETTE};
use cc_core::ComponentKind;
use cc_engine::commands::io;
use egui::{Align, Context, CornerRadius, Layout, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Ui, vec2};
use glam::DVec2;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Where the window's components come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Source {
    /// The folders of the preferences.
    #[default]
    Folders,
    /// The 3D CraftHouse library, online.
    Online,
    /// A Plugin API 3 catalog provider (index into `state.plugins.catalogs`).
    PluginCatalog(usize),
}

/// The order of the cards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sort {
    /// By name, ignoring case.
    #[default]
    Name,
    /// By kind, then by name.
    Kind,
}

impl Sort {
    const ALL: [Sort; 2] = [Sort::Name, Sort::Kind];

    fn label(self) -> String {
        match self {
            Sort::Name => t!("lib-sort-name"),
            Sort::Kind => t!("lib-sort-kind"),
        }
    }
}

/// A component file found in a library folder (from `component_library_list`)
/// or listed by the online catalog.
#[derive(Debug, Clone, Default)]
pub struct Entry {
    /// The file; online, its path in the catalog.
    pub path: PathBuf,
    pub component: String,
    pub kind: String,
    /// Who made it (online only).
    pub author: String,
    pub types: Vec<(String, String)>,
    pub parameters: Vec<String>,
    pub connectors: Vec<String>,
    /// The picture the online catalog gives for it, as a path under the
    /// library (checked by [`crate::crafthouse::parse_catalog`]); `None`
    /// for a folder's component.
    pub thumbnail: Option<PathBuf>,
}

/// State of the open library window.
#[derive(Debug, Clone, Default)]
pub struct Library {
    pub source: Source,
    pub entries: Vec<Entry>,
    /// Files that could not be read.
    pub errors: usize,
    pub search: String,
    /// Only this kind (by its `Debug` name), or all.
    pub kind: Option<String>,
    /// Only the components under this library folder (My Folders), or all.
    pub folder: Option<PathBuf>,
    /// Only the components of this author (3D CraftHouse), or all.
    pub author: Option<String>,
    pub sort: Sort,
    pub selected: Option<PathBuf>,
    /// Active category filter for a plugin catalog source.
    pub catalog_category: Option<String>,
    /// Active color filter for a plugin catalog source.
    pub catalog_color: Option<String>,
    /// Optional max dimension filters (in project units) for a plugin catalog source.
    pub catalog_max_w: String,
    pub catalog_max_d: String,
    pub catalog_max_h: String,
    /// Selected item id for a plugin catalog source.
    pub catalog_selected: Option<String>,
    /// Plan outlines of the selected component (for its thumbnail).
    pub outlines: Vec<Vec<DVec2>>,
    /// The row that saves a project component into a library folder is shown.
    pub saving: bool,
    /// The project component to save into the library, and the folder.
    pub save_component: Option<String>,
    pub save_folder: Option<PathBuf>,
    /// The folders the entries were read from (read again when the preference changes).
    listed: Vec<PathBuf>,
    /// The component files whose thumbnail was asked for since the window opened.
    thumbnails_asked: std::collections::BTreeSet<PathBuf>,
}

/// Width and height of the window's body, when the screen has room for them.
const BODY: egui::Vec2 = vec2(1120.0, 600.0);
/// Width of the filter column.
const SIDEBAR: f32 = 200.0;
/// Width of the details column.
const DETAILS: f32 = 280.0;
/// Width of the space between two columns, with its line in the middle.
const DIVIDER: f32 = 17.0;
/// Width a card is laid out near, and the space between cards.
const CARD: f32 = 148.0;
const GAP: f32 = 12.0;
/// Height of a card under its picture: the name and the kind.
const CARD_TEXT: f32 = 46.0;
const CARD_RADIUS: u8 = 6;
/// Height of the source switch and the search field over the columns.
const TOP_BAR: f32 = 38.0;
/// Height of the row that saves a project component, under the columns.
const SAVE_ROW: f32 = 44.0;

/// Open the library window, reading the folders.
pub fn open(state: &mut AppState) {
    let mut lib = Library::default();
    refresh(state, &mut lib);
    state.dialogs.library = Some(lib);
}

/// Read the folders again through the engine.
fn refresh(state: &mut AppState, lib: &mut Library) {
    let folders = state.prefs.library_folders.clone();
    lib.listed = folders.clone();
    let out = state.session.run(io::ComponentLibraryList { folders, search: None });
    let Ok(out) = out else {
        lib.entries.clear();
        return;
    };
    let text = |v: &serde_json::Value| v.as_str().unwrap_or_default().to_string();
    lib.entries = out["components"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| Entry {
            path: PathBuf::from(text(&c["path"])),
            component: text(&c["component"]),
            kind: text(&c["kind"]),
            author: String::new(),
            types: c["types"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|t| (text(&t["name"]), text(&t["description"])))
                .collect(),
            parameters: c["parameters"].as_array().into_iter().flatten().map(text).collect(),
            connectors: c["connectors"].as_array().into_iter().flatten().map(text).collect(),
            thumbnail: None,
        })
        .collect();
    lib.errors = out["errors"].as_array().map_or(0, Vec::len);
    if lib.selected.as_ref().is_some_and(|p| !lib.entries.iter().any(|e| &e.path == p)) {
        lib.selected = None;
        lib.outlines.clear();
    }
}

/// Plan outlines of the component in `path` (its first type), for the thumbnail.
fn outlines(path: &Path) -> Vec<Vec<DVec2>> {
    let Ok(file) = cc_core::component_file::load(path) else { return Vec::new() };
    let thickness = [(cc_core::component::WALL_THICKNESS, 0.15)];
    let parts = cc_geom::resolve_component_in(&file.def, &Default::default(), &thickness);
    let (in_plan, all): (Vec<_>, Vec<_>) = parts.into_iter().partition(|p| p.in_plan);
    // Components with no piece marked for plans show all their outlines.
    let shown = if in_plan.is_empty() { all } else { in_plan };
    shown.into_iter().filter(|p| !p.void).map(|p| p.footprint).collect()
}

fn kind_label(kind: &str) -> String {
    ComponentKind::ALL.into_iter().find(|k| format!("{k:?}") == kind).map_or_else(|| kind.to_string(), |k| tr(k.key()))
}

/// Show the components of `source`: the folders read again, or the online
/// catalog as far as it is read. The folder and author filters belong to
/// one source and are cleared.
fn set_source(state: &mut AppState, lib: &mut Library, source: Source) {
    lib.source = source;
    lib.selected = None;
    lib.catalog_selected = None;
    lib.catalog_category = None;
    lib.catalog_color = None;
    lib.outlines.clear();
    lib.folder = None;
    lib.author = None;
    match source {
        Source::Folders => refresh(state, lib),
        Source::Online => {
            lib.entries = state.crafthouse.entries();
            lib.errors = 0;
        }
        Source::PluginCatalog(_) => {
            lib.entries.clear();
            lib.errors = 0;
        }
    }
}

/// Forget the online catalog and the downloads, and read the catalog again.
fn read_online_again(state: &mut AppState, lib: &mut Library) {
    state.crafthouse.reset();
    set_source(state, lib, Source::Online);
}

/// The file to load for the component picked: online, once it is downloaded.
fn picked_file(state: &AppState, lib: &Library) -> Option<PathBuf> {
    let selected = lib.selected.as_ref()?;
    match lib.source {
        Source::Folders => Some(selected.clone()),
        Source::Online => state.crafthouse.local(selected).cloned(),
        Source::PluginCatalog(_) => None,
    }
}

/// Pick the component of `path`; its plan shows at once (online, once its
/// file is downloaded).
fn select(state: &AppState, lib: &mut Library, path: PathBuf) {
    lib.outlines = match lib.source {
        Source::Folders => outlines(&path),
        Source::Online => state.crafthouse.local(&path).map(|p| outlines(p)).unwrap_or_default(),
        Source::PluginCatalog(_) => Vec::new(),
    };
    lib.selected = Some(path);
}

pub fn show(ctx: &Context, state: &mut AppState) {
    let Some(mut lib) = state.dialogs.library.take() else { return };
    if let Source::PluginCatalog(idx) = lib.source
        && idx >= state.plugins.catalogs.len()
    {
        set_source(state, &mut lib, Source::Folders);
    }
    // The window keeps clear of the screen's edges on small screens.
    let screen = ctx.content_rect().size();
    let size = vec2((screen.x - 80.0).clamp(720.0, BODY.x), (screen.y - 190.0).clamp(360.0, BODY.y));
    let can_save = lib.source == Source::Folders && can_save(state);
    let saving = lib.saving;
    // Nothing to load until a component is picked and, online, its file has arrived.
    let waiting = match lib.source {
        Source::PluginCatalog(_) => lib.catalog_selected.is_none() || state.plugin_catalog.preparing.is_some(),
        _ => picked_file(state, &lib).is_none(),
    };
    let (mut load, mut save) = (0, None);
    let mut open_now = false;
    let mut prepared_ready = None;
    let response = Dialog::new("component-library", t!("dlg-library")).width(size.x).show(
        ctx,
        |ui| body(ui, state, &mut lib, size, can_save, &mut open_now, &mut prepared_ready),
        |f| {
            if can_save {
                save = Some(f.toggle(t!("lib-save-toggle"), saving));
            }
            load = f.primary(t!("lib-load"));
            if waiting {
                f.disable(load);
            }
            f.secondary(t!("action-close"));
        },
    );
    if let Some(prepared) = prepared_ready {
        crate::actions::load_catalog_component(state, prepared);
        return;
    }
    if response.dismissed {
        return;
    }
    match response.clicked {
        Some(i) if i == load => open_now = true,
        Some(i) if Some(i) == save => lib.saving = !lib.saving,
        Some(_) => return,
        None => {}
    }
    // Load into Project, or a card double-clicked.
    if open_now {
        if let Source::PluginCatalog(_) = lib.source {
            if let Some(id) = lib.catalog_selected.clone() {
                state.plugin_catalog.request_prepare(&id);
            }
        } else if let Some(path) = picked_file(state, &lib) {
            crate::actions::load_component_path(state, path);
            return;
        }
    }
    state.dialogs.library = Some(lib);
}

fn body(
    ui: &mut Ui,
    state: &mut AppState,
    lib: &mut Library,
    size: egui::Vec2,
    can_save: bool,
    open_now: &mut bool,
    prepared_ready: &mut Option<crate::plugin_catalog::PreparedLoad>,
) {
    match lib.source {
        // The folders changed in Preferences: read them again.
        Source::Folders if lib.listed != state.prefs.library_folders => refresh(state, lib),
        Source::Folders => {}
        // Read the catalog and download the pictures of the cards in sight and
        // the component picked (see `CraftHouse::sync`).
        Source::Online => {
            let (read, arrived) = state.crafthouse.sync(ui.ctx(), lib.selected.as_deref());
            if let Some(entries) = read {
                lib.entries = entries;
            }
            if let Some(path) = arrived {
                lib.outlines = outlines(&path);
            }
        }
        Source::PluginCatalog(idx) => {
            if let Some(cat) = state.plugins.catalogs.get(idx).cloned()
                && let Some(listed) = state.plugins.listed.iter().find(|l| l.id == cat.plugin_id).cloned()
            {
                let folder = state.prefs.plugins_dir();
                let net = state.prefs.plugins_network.contains(&cat.plugin_id);
                state.plugin_catalog.activate(ui.ctx(), &cat.plugin_id, &cat.id, &listed.dir, folder.as_deref(), net);
            }
            let parse_dim = |s: &str| {
                let s = s.trim();
                (!s.is_empty()).then(|| cc_views::format::parse_length(s)).flatten().filter(|v| *v > 0.0)
            };
            let query = crate::plugin_catalog::CatalogQuery {
                text: lib.search.clone(),
                category: lib.catalog_category.clone(),
                color: lib.catalog_color.clone(),
                max_width: parse_dim(&lib.catalog_max_w),
                max_depth: parse_dim(&lib.catalog_max_d),
                max_height: parse_dim(&lib.catalog_max_h),
            };
            if let Some(prepared) = state.plugin_catalog.sync(&query) {
                *prepared_ready = Some(prepared);
            }
        }
    }
    top_bar(ui, state, lib);
    let save_row = lib.saving && can_save && lib.source == Source::Folders;
    let height = size.y - TOP_BAR - if save_row { SAVE_ROW } else { 0.0 };
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let grid_width = (size.x - SIDEBAR - DETAILS - 2.0 * DIVIDER).max(CARD);
        if let Source::PluginCatalog(idx) = lib.source {
            column(ui, SIDEBAR, height, |ui| plugin_catalog_sidebar(ui, state, lib, idx));
            divider(ui, height);
            column(ui, grid_width, height, |ui| plugin_catalog_grid(ui, state, lib, open_now));
            divider(ui, height);
            column(ui, DETAILS, height, |ui| plugin_catalog_details(ui, state, lib));
        } else {
            column(ui, SIDEBAR, height, |ui| sidebar(ui, state, lib));
            divider(ui, height);
            column(ui, grid_width, height, |ui| grid(ui, state, lib, open_now));
            divider(ui, height);
            column(ui, DETAILS, height, |ui| details(ui, state, lib));
        }
    });
    if save_row {
        save_section(ui, state, lib);
    }
}

/// The source switch, the search field and the order of the cards.
fn top_bar(ui: &mut Ui, state: &mut AppState, lib: &mut Library) {
    let mut source = lib.source;
    ui.horizontal(|ui| {
        ui.selectable_value(&mut source, Source::Folders, t!("lib-source-folders"));
        ui.selectable_value(&mut source, Source::Online, t!("lib-source-online"));
        for (i, cat) in state.plugins.catalogs.iter().enumerate() {
            ui.selectable_value(&mut source, Source::PluginCatalog(i), &cat.label);
        }
        ui.add_space(12.0);
        // Narrower on a small screen, leaving room for the order.
        let width = (ui.available_width() - 280.0).clamp(140.0, 320.0);
        ui.add(egui::TextEdit::singleline(&mut lib.search).hint_text(t!("lib-search")).desired_width(width));
        if !matches!(lib.source, Source::PluginCatalog(_)) {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                egui::ComboBox::from_id_salt("lib-sort").selected_text(lib.sort.label()).width(110.0).show_ui(
                    ui,
                    |ui| {
                        for sort in Sort::ALL {
                            ui.selectable_value(&mut lib.sort, sort, sort.label());
                        }
                    },
                );
                ui.label(theme::muted(t!("lib-sort")));
            });
        }
    });
    if source != lib.source {
        set_source(state, lib, source);
    }
    ui.add_space(2.0);
}

/// A column `width` wide and `height` high; what it shows scrolls in it.
fn column(ui: &mut Ui, width: f32, height: f32, add: impl FnOnce(&mut Ui)) {
    ui.allocate_ui_with_layout(vec2(width, height), Layout::top_down(Align::Min), |ui| {
        ui.set_width(width);
        ui.set_height(height);
        add(ui);
    });
}

/// The line between two columns.
fn divider(ui: &mut Ui, height: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(DIVIDER, height), Sense::hover());
    ui.painter().vline(rect.center().x, rect.y_range(), Stroke::new(1.0, PALETTE.border));
}

/// Whether the search or a filter leaves components out.
fn narrowed(lib: &Library) -> bool {
    !lib.search.trim().is_empty() || lib.kind.is_some() || lib.folder.is_some() || lib.author.is_some()
}

fn clear_filters(lib: &mut Library) {
    lib.search.clear();
    lib.kind = None;
    lib.folder = None;
    lib.author = None;
}

/// The entries the search and the filters leave, in the order picked.
fn shown(lib: &Library) -> Vec<usize> {
    let query = lib.search.trim().to_lowercase();
    let wanted = |e: &Entry| {
        lib.kind.as_ref().is_none_or(|k| &e.kind == k)
            && match lib.source {
                Source::Folders => lib.folder.as_ref().is_none_or(|f| e.path.starts_with(f)),
                Source::Online => lib.author.as_ref().is_none_or(|a| &e.author == a),
                Source::PluginCatalog(_) => true,
            }
            && (query.is_empty()
                || e.component.to_lowercase().contains(&query)
                || e.types.iter().any(|(n, d)| n.to_lowercase().contains(&query) || d.to_lowercase().contains(&query)))
    };
    let mut labels: BTreeMap<&str, String> = BTreeMap::new();
    let mut keyed: Vec<((String, String, &Path), usize)> = Vec::new();
    for (i, e) in lib.entries.iter().enumerate().filter(|(_, e)| wanted(e)) {
        let kind = match lib.sort {
            Sort::Name => String::new(),
            Sort::Kind => labels.entry(e.kind.as_str()).or_insert_with(|| kind_label(&e.kind).to_lowercase()).clone(),
        };
        keyed.push(((kind, e.component.to_lowercase(), &e.path), i));
    }
    keyed.sort();
    keyed.into_iter().map(|(_, i)| i).collect()
}

/// The filters, then the source's buttons and how its reading stands, at
/// the bottom of the column.
fn sidebar(ui: &mut Ui, state: &mut AppState, lib: &mut Library) {
    ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
        // Laid out from the bottom up: the last shown first.
        let full = vec2(ui.available_width(), 26.0);
        match lib.source {
            Source::Folders => {
                if ui.add(egui::Button::new(t!("lib-refresh")).min_size(full)).clicked() {
                    refresh(state, lib);
                }
                if ui.add(egui::Button::new(t!("lib-manage-folders")).min_size(full)).clicked() {
                    crate::preferences::open(state, crate::preferences::Section::FileLocations);
                }
            }
            Source::Online => {
                ui.add(egui::Label::new(theme::muted(t!("lib-online-about")).size(11.5)).wrap());
                match state.crafthouse.status() {
                    Status::Reading => {
                        ui.horizontal_wrapped(|ui| {
                            ui.spinner();
                            ui.label(theme::muted(t!("lib-online-reading")).size(12.0));
                        });
                    }
                    Status::Failed(message) => {
                        let text = t!("lib-online-failed", "message" => message);
                        ui.add(egui::Label::new(RichText::new(text).color(PALETTE.warning).size(12.0)).wrap());
                    }
                    Status::Ready(_) => {}
                }
                ui.add_space(4.0);
                if ui.add(egui::Button::new(t!("lib-refresh")).min_size(full)).clicked() {
                    read_online_again(state, lib);
                }
                if ui.add(egui::Button::new(t!("lib-online-site")).min_size(full)).clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(crate::crafthouse::SITE));
                }
            }
            Source::PluginCatalog(_) => {}
        }
        ui.add_space(6.0);
        ui.separator();
        ui.with_layout(Layout::top_down(Align::Min), |ui| {
            egui::ScrollArea::vertical()
                .id_salt("lib-filters")
                .auto_shrink([false, false])
                .show(ui, |ui| filters(ui, state, lib));
        });
    });
}

/// The kinds of the source's components, then its folders or its authors,
/// each a row that shows only those (with how many there are).
fn filters(ui: &mut Ui, state: &AppState, lib: &mut Library) {
    section_title(ui, t!("lib-category"));
    let total = lib.entries.len();
    if filter_row(ui, &t!("lib-all-kinds"), total, lib.kind.is_none()).clicked() {
        lib.kind = None;
    }
    for (kind, count) in kinds(lib) {
        let selected = lib.kind.as_ref() == Some(&kind);
        if filter_row(ui, &kind_label(&kind), count, selected).clicked() {
            lib.kind = Some(kind);
        }
    }
    match lib.source {
        Source::Folders => {
            let folders = &state.prefs.library_folders;
            if lib.folder.as_ref().is_some_and(|f| !folders.contains(f)) {
                lib.folder = None;
            }
            if folders.len() < 2 {
                return;
            }
            ui.add_space(10.0);
            section_title(ui, t!("lib-folder"));
            if filter_row(ui, &t!("lib-all-folders"), total, lib.folder.is_none()).clicked() {
                lib.folder = None;
            }
            for folder in folders {
                let name =
                    folder.file_name().map_or_else(|| folder.display().to_string(), |n| n.to_string_lossy().into());
                let count = lib.entries.iter().filter(|e| e.path.starts_with(folder)).count();
                let row = filter_row(ui, &name, count, lib.folder.as_ref() == Some(folder));
                if row.on_hover_text(folder.display().to_string()).clicked() {
                    lib.folder = Some(folder.clone());
                }
            }
        }
        Source::Online => {
            let mut authors: BTreeMap<&str, usize> = BTreeMap::new();
            for e in lib.entries.iter().filter(|e| !e.author.trim().is_empty()) {
                *authors.entry(e.author.as_str()).or_default() += 1;
            }
            let mut authors: Vec<(String, usize)> = authors.into_iter().map(|(a, n)| (a.to_string(), n)).collect();
            authors.sort_by_cached_key(|(a, _)| (a.to_lowercase(), a.clone()));
            if lib.author.as_ref().is_some_and(|a| !authors.iter().any(|(b, _)| a == b)) {
                lib.author = None;
            }
            if authors.len() < 2 {
                return;
            }
            ui.add_space(10.0);
            section_title(ui, t!("lib-author"));
            if filter_row(ui, &t!("lib-all-authors"), total, lib.author.is_none()).clicked() {
                lib.author = None;
            }
            for (author, count) in authors {
                let selected = lib.author.as_ref() == Some(&author);
                if filter_row(ui, &author, count, selected).clicked() {
                    lib.author = Some(author);
                }
            }
        }
        Source::PluginCatalog(_) => {}
    }
}

/// The kinds of the entries and how many each has: the app's kinds in
/// their order, then others a catalog names. The kind picked stays listed
/// when no entry has it, so it can be seen and cleared.
fn kinds(lib: &Library) -> Vec<(String, usize)> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for e in lib.entries.iter().filter(|e| !e.kind.trim().is_empty()) {
        *counts.entry(e.kind.as_str()).or_default() += 1;
    }
    if let Some(kind) = &lib.kind {
        counts.entry(kind.as_str()).or_default();
    }
    let known: Vec<String> = ComponentKind::ALL.iter().map(|k| format!("{k:?}")).collect();
    let mut kinds: Vec<(String, usize)> =
        known.iter().filter_map(|k| counts.get(k.as_str()).map(|n| (k.clone(), *n))).collect();
    kinds.extend(
        counts.into_iter().filter(|(k, _)| !known.iter().any(|n| n.as_str() == *k)).map(|(k, n)| (k.to_string(), n)),
    );
    kinds
}

/// The heading of a group of filters.
fn section_title(ui: &mut Ui, text: String) {
    ui.add_space(2.0);
    let text = RichText::new(text).family(theme::semibold()).size(11.5).color(PALETTE.text_muted);
    ui.add(egui::Label::new(text).truncate());
}

/// A row of the filters: its name, cut to fit, and how many components it
/// shows, at the right.
fn filter_row(ui: &mut Ui, label: &str, count: usize, selected: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::click());
    response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, label));
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let painter = ui.painter();
    let fill = if selected {
        Some(PALETTE.accent_soft)
    } else if response.hovered() {
        Some(PALETTE.hover)
    } else {
        None
    };
    if let Some(fill) = fill {
        painter.rect_filled(rect, theme::RADIUS as f32, fill);
    }
    let inner = rect.shrink2(vec2(8.0, 0.0));
    let number = painter.layout_no_wrap(count.to_string(), egui::FontId::proportional(12.0), PALETTE.text_muted);
    let color = if selected { PALETTE.accent_text } else { PALETTE.text };
    let width = (inner.width() - number.size().x - 8.0).max(0.0);
    let text = one_line(ui, RichText::new(label).size(13.0).color(color), width);
    painter.galley(Pos2::new(inner.left(), rect.center().y - text.size().y / 2.0), text, color);
    let at = Pos2::new(inner.right() - number.size().x, rect.center().y - number.size().y / 2.0);
    painter.galley(at, number, PALETTE.text_muted);
    response
}

/// `text` on one line at most `width` wide, cut with an ellipsis.
fn one_line(ui: &Ui, text: RichText, width: f32) -> std::sync::Arc<egui::Galley> {
    egui::WidgetText::from(text).into_galley(ui, Some(egui::TextWrapMode::Truncate), width, egui::TextStyle::Body)
}

/// How many components are shown, over the cards; the cards, or why there
/// are none.
fn grid(ui: &mut Ui, state: &mut AppState, lib: &mut Library, open_now: &mut bool) {
    let status = match lib.source {
        Source::Folders | Source::PluginCatalog(_) => None,
        Source::Online => Some(state.crafthouse.status()),
    };
    match status {
        Some(Status::Reading) => {
            centered(ui, |ui| {
                ui.spinner();
                ui.label(theme::muted(t!("lib-online-reading")));
            });
            return;
        }
        Some(Status::Failed(message)) => {
            centered(ui, |ui| {
                let text = t!("lib-online-failed", "message" => message);
                ui.add(egui::Label::new(RichText::new(text).color(PALETTE.warning)).wrap());
                if ui.button(t!("lib-try-again")).clicked() {
                    read_online_again(state, lib);
                }
            });
            return;
        }
        _ => {}
    }
    if lib.source == Source::Folders && state.prefs.library_folders.is_empty() {
        centered(ui, |ui| {
            ui.add(egui::Label::new(theme::muted(t!("lib-empty"))).wrap());
            if ui.button(t!("lib-manage-folders")).clicked() {
                crate::preferences::open(state, crate::preferences::Section::FileLocations);
            }
        });
        return;
    }
    let shown = shown(lib);
    let total = lib.entries.len();
    let narrowed = narrowed(lib);
    if total > 0 || lib.errors > 0 {
        ui.horizontal_wrapped(|ui| {
            let count = if narrowed {
                t!("lib-count-of", "shown" => shown.len(), "total" => total)
            } else {
                t!("lib-count", "count" => total)
            };
            ui.label(theme::muted(count).size(12.0));
            if lib.errors > 0 {
                ui.label(RichText::new(t!("lib-errors", "count" => lib.errors)).color(PALETTE.warning).size(12.0));
            }
        });
    }
    if shown.is_empty() {
        centered(ui, |ui| {
            ui.label(theme::muted(t!("lib-none")));
            if narrowed && ui.button(t!("lib-clear-filters")).clicked() {
                clear_filters(lib);
            }
        });
        return;
    }
    // As many cards in a row as fit near their width, widened to fill it.
    let width = (ui.available_width() - ui.spacing().scroll.allocated_width() - 6.0).max(60.0);
    let columns = (((width + GAP) / (CARD + GAP)).round() as usize).max(1);
    let card = vec2(((width - GAP * (columns - 1) as f32) / columns as f32).max(40.0), 0.0);
    let card = vec2(card.x, card.x * 2.0 / 3.0 + CARD_TEXT);
    let rows = shown.len().div_ceil(columns);
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing = vec2(GAP, GAP);
        // Only the rows in sight are laid out: a library may hold thousands of components.
        egui::ScrollArea::vertical().id_salt("lib-grid").auto_shrink([false, false]).show_rows(
            ui,
            card.y,
            rows,
            |ui, range| {
                for row in range {
                    ui.horizontal(|ui| {
                        for &i in shown.iter().skip(row * columns).take(columns) {
                            card_ui(ui, state, lib, i, card, open_now);
                        }
                    });
                }
            },
        );
    });
}

/// What the middle column shows when it has no cards, in its middle.
fn centered(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    ui.add_space((ui.available_height() * 0.35).max(0.0));
    ui.vertical_centered(add);
}

/// The card of the entry `index`: its 3D thumbnail, name and kind. A click
/// picks it, a double click loads it too.
fn card_ui(ui: &mut Ui, state: &mut AppState, lib: &mut Library, index: usize, size: egui::Vec2, open_now: &mut bool) {
    let Some(e) = lib.entries.get(index) else { return };
    let (path, name, kind, types) = (e.path.clone(), e.component.clone(), kind_label(&e.kind), e.types.len());
    let catalog_picture = e.thumbnail.clone();
    let selected = lib.selected.as_ref() == Some(&path);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let label = format!("{name}  ·  {kind}");
    response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, &label));
    let picture_rect = Rect::from_min_size(rect.min, vec2(size.x, size.x * 2.0 / 3.0));
    let mut failed = None;
    if ui.is_rect_visible(rect) {
        ui.painter().rect_filled(rect, CARD_RADIUS, PALETTE.canvas);
        // Only the cards in sight get a thumbnail; online, their catalog pictures are downloaded for it.
        let (shown, file) = match lib.source {
            Source::Folders => (None, Some(path.clone())),
            Source::Online => {
                if let FileStatus::Failed(message) = state.crafthouse.file_status(&path) {
                    failed = Some(message);
                }
                online_picture(state, &path, catalog_picture.as_deref())
            }
            Source::PluginCatalog(_) => (None, None),
        };
        let corners = CornerRadius { nw: CARD_RADIUS - 1, ne: CARD_RADIUS - 1, sw: 0, se: 0 };
        picture(ui, state, lib, picture_rect.shrink(1.0), shown.as_deref(), file.as_deref(), corners);
        let painter = ui.painter();
        painter.hline(rect.x_range(), picture_rect.bottom(), Stroke::new(1.0, PALETTE.border));
        let text_width = size.x - 16.0;
        let title = one_line(ui, RichText::new(&name).family(theme::semibold()).size(13.0), text_width);
        painter.galley(Pos2::new(rect.left() + 8.0, picture_rect.bottom() + 7.0), title, PALETTE.text);
        let line = if types > 1 && !kind.is_empty() {
            t!("lib-card-line", "kind" => kind.clone(), "count" => types)
        } else {
            kind.clone()
        };
        let line = one_line(ui, RichText::new(line).size(12.0).color(PALETTE.text_muted), text_width);
        painter.galley(Pos2::new(rect.left() + 8.0, picture_rect.bottom() + 25.0), line, PALETTE.text_muted);
        if failed.is_some() {
            let center = Pos2::new(picture_rect.right() - 13.0, picture_rect.top() + 13.0);
            painter.circle_filled(center, 8.0, PALETTE.warning);
            let font = egui::FontId::new(11.0, theme::semibold());
            painter.text(center, egui::Align2::CENTER_CENTER, "!", font, egui::Color32::WHITE);
        }
        let stroke = if selected {
            Stroke::new(2.0, PALETTE.accent)
        } else if response.hovered() {
            Stroke::new(1.0, PALETTE.border_strong)
        } else {
            Stroke::new(1.0, PALETTE.border)
        };
        painter.rect_stroke(rect, CARD_RADIUS, stroke, StrokeKind::Inside);
    }
    // The warning mark says why the file did not download; the rest of the card, what it is.
    let mark =
        Rect::from_center_size(Pos2::new(picture_rect.right() - 13.0, picture_rect.top() + 13.0), vec2(20.0, 20.0));
    let on_mark = response.hover_pos().is_some_and(|p| mark.contains(p));
    let tip = match failed {
        Some(message) if on_mark => t!("lib-online-download-failed", "message" => message),
        _ => format!("{name}\n{}", path.display()),
    };
    let response = response.on_hover_text(tip);
    if response.clicked() && !selected {
        select(state, lib, path);
    }
    if response.double_clicked() {
        *open_now = true;
    }
}

/// What an online component shows: the local copy of its catalog
/// `picture` once it has arrived (asked for here, so only for what is in
/// sight), and the local file of the component `file` when it is there
/// (downloaded, or kept from an earlier run; never downloaded for this).
fn online_picture(state: &mut AppState, file: &Path, picture: Option<&Path>) -> (Option<PathBuf>, Option<PathBuf>) {
    let shown = picture.and_then(|p| {
        state.crafthouse.want_picture(p);
        state.crafthouse.picture(p).cloned()
    });
    (shown, state.crafthouse.local_or_kept(file))
}

/// In `rect`, the picture `shown` (an online component's, from its
/// catalog); else the 3D thumbnail of the component file `file`; else a
/// placeholder: the picture has not arrived, the file is not there, its
/// thumbnail is still to be rendered (asked here, once per window), or it
/// has nothing to show in 3D.
fn picture(
    ui: &Ui,
    state: &mut AppState,
    lib: &mut Library,
    rect: Rect,
    shown: Option<&Path>,
    file: Option<&Path>,
    corners: CornerRadius,
) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, corners, PALETTE.paper);
    let thumbnail = file.and_then(crate::thumbnails::component_file);
    let texture = shown
        .and_then(|p| crate::home::load_thumbnail(ui.ctx(), p))
        .or_else(|| thumbnail.as_deref().filter(|p| p.exists()).and_then(|p| crate::home::load_thumbnail(ui.ctx(), p)));
    if let Some(texture) = texture {
        let uv = Rect::from_min_max(Pos2::ZERO, egui::pos2(1.0, 1.0));
        painter.image(texture.id(), rect.shrink(1.0), uv, egui::Color32::WHITE);
        return;
    }
    let side = (rect.width().min(rect.height()) * 0.3).clamp(16.0, 56.0);
    let icon = Rect::from_center_size(rect.center(), vec2(side, side));
    crate::icons::paint(&painter, icon, crate::icons::Icon::Component, PALETTE.border_strong);
    if let (Some(file), Some(_)) = (file, thumbnail)
        && lib.thumbnails_asked.insert(file.to_path_buf())
    {
        state.thumbnail_jobs.push(crate::thumbnails::Job::Component(file.to_path_buf()));
    }
}

/// The component picked: its 3D thumbnail, name, origin, plan, download,
/// types, parameters and connectors.
fn details(ui: &mut Ui, state: &mut AppState, lib: &mut Library) {
    let Some(e) = lib.selected.as_ref().and_then(|p| lib.entries.iter().find(|e| &e.path == p)).cloned() else {
        centered(ui, |ui| {
            ui.add(egui::Label::new(theme::muted(t!("lib-pick"))).wrap());
        });
        return;
    };
    egui::ScrollArea::vertical().id_salt("lib-details").auto_shrink([false, false]).show(ui, |ui| {
        // Clear of the scroll bar.
        let width = (ui.available_width() - 8.0).max(40.0);
        ui.set_width(width);
        let (rect, _) = ui.allocate_exact_size(vec2(width, width * 2.0 / 3.0), Sense::hover());
        let (shown, file) = match lib.source {
            Source::Folders => (None, Some(e.path.clone())),
            Source::Online => online_picture(state, &e.path, e.thumbnail.as_deref()),
            Source::PluginCatalog(_) => (None, None),
        };
        picture(ui, state, lib, rect, shown.as_deref(), file.as_deref(), CornerRadius::same(4));
        ui.painter().rect_stroke(rect, 4.0, Stroke::new(1.0, PALETTE.border), StrokeKind::Inside);
        ui.add_space(4.0);
        ui.add(egui::Label::new(theme::title(&e.component)).wrap());
        // A folder's component says where its file is; an online one, who made it.
        let origin = if lib.source == Source::Online { e.author.clone() } else { e.path.display().to_string() };
        let line = if origin.is_empty() { kind_label(&e.kind) } else { format!("{} · {origin}", kind_label(&e.kind)) };
        ui.add(egui::Label::new(theme::muted(line).size(11.5)).truncate());
        ui.add_space(4.0);
        let (rect, _) = ui.allocate_exact_size(vec2(width, 110.0), Sense::hover());
        thumbnail(ui, rect, &lib.outlines);
        if lib.source == Source::Online {
            let text = match state.crafthouse.file_status(&e.path) {
                FileStatus::Downloading => Some(theme::muted(t!("lib-online-downloading"))),
                FileStatus::Failed(message) => {
                    Some(RichText::new(t!("lib-online-download-failed", "message" => message)).color(PALETTE.warning))
                }
                FileStatus::Ready(_) => None,
            };
            if let Some(text) = text {
                ui.add(egui::Label::new(text.size(12.0)).truncate());
            }
        }
        ui.add_space(6.0);
        ui.label(RichText::new(t!("lib-types")).strong());
        for (name, description) in &e.types {
            let text = if description.is_empty() { name.clone() } else { format!("{name} — {description}") };
            ui.add(egui::Label::new(text).wrap());
        }
        if !e.parameters.is_empty() {
            ui.add_space(4.0);
            ui.label(RichText::new(t!("comp-params")).strong());
            ui.add(egui::Label::new(e.parameters.join(", ")).wrap());
        }
        if !e.connectors.is_empty() {
            ui.add_space(4.0);
            ui.label(RichText::new(t!("comp-connectors")).strong());
            ui.add(egui::Label::new(e.connectors.join(", ")).wrap());
        }
    });
}

/// The component's plan outlines, fitted in `rect`.
fn thumbnail(ui: &Ui, rect: egui::Rect, outlines: &[Vec<DVec2>]) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, PALETTE.paper);
    let pts = outlines.iter().flatten();
    let Some((lo, hi)) = pts
        .fold(None, |acc: Option<(DVec2, DVec2)>, p| Some(acc.map_or((*p, *p), |(lo, hi)| (lo.min(*p), hi.max(*p)))))
    else {
        return;
    };
    let inner = rect.shrink(14.0);
    let extent = (hi - lo).max(DVec2::splat(1e-3));
    let k = (inner.width() as f64 / extent.x).min(inner.height() as f64 / extent.y);
    let center = (lo + hi) * 0.5;
    let to = |p: DVec2| {
        let q = (p - center) * k;
        Pos2::new(inner.center().x + q.x as f32, inner.center().y - q.y as f32)
    };
    let stroke = Stroke::new(1.0, egui::Color32::from_gray(50));
    for outline in outlines {
        let mut points: Vec<Pos2> = outline.iter().map(|p| to(*p)).collect();
        if let Some(first) = points.first().copied() {
            points.push(first);
        }
        painter.add(egui::Shape::line(points, stroke));
    }
}

/// The names of the project's components, sorted.
fn project_components(state: &AppState) -> Vec<String> {
    let mut components: Vec<String> = state
        .project()
        .component_types()
        .filter_map(|t| match &t.data {
            cc_core::TypeData::Component { component, .. } => Some(component.clone()),
            _ => None,
        })
        .collect();
    components.sort();
    components.dedup();
    components
}

/// Whether a project component can be saved: the project has one and
/// there is a library folder to save it into.
fn can_save(state: &AppState) -> bool {
    !state.prefs.library_folders.is_empty() && !project_components(state).is_empty()
}

/// Save one of the project's components into a library folder.
fn save_section(ui: &mut Ui, state: &mut AppState, lib: &mut Library) {
    let components = project_components(state);
    if components.is_empty() || state.prefs.library_folders.is_empty() {
        return;
    }
    ui.separator();
    // Wrapped: the labels and a long folder name must not widen the dialog.
    ui.horizontal_wrapped(|ui| {
        ui.label(theme::muted(t!("lib-save")));
        let selected = lib.save_component.clone().unwrap_or_default();
        egui::ComboBox::from_id_salt("lib-save-component").selected_text(selected).width(170.0).truncate().show_ui(
            ui,
            |ui| {
                for c in &components {
                    ui.selectable_value(&mut lib.save_component, Some(c.clone()), c);
                }
            },
        );
        ui.label(theme::muted(t!("lib-save-into")));
        let folder = lib.save_folder.clone().or_else(|| state.prefs.library_folders.first().cloned());
        let folder_name = folder.as_ref().map(|f| f.display().to_string()).unwrap_or_default();
        egui::ComboBox::from_id_salt("lib-save-folder").selected_text(folder_name).width(180.0).truncate().show_ui(
            ui,
            |ui| {
                for f in &state.prefs.library_folders {
                    ui.selectable_value(&mut lib.save_folder, Some(f.clone()), f.display().to_string());
                }
            },
        );
        let ready = lib.save_component.is_some() && folder.is_some();
        if ui.add_enabled(ready, egui::Button::new(t!("lib-save-button"))).clicked()
            && let (Some(component), Some(folder)) = (lib.save_component.clone(), folder)
        {
            let file: String =
                component.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == ' ' { c } else { '_' }).collect();
            let path = folder.join(format!("{}.ccomp", file.trim()));
            match state.session.run(io::ComponentSave { component, path: path.clone() }) {
                Ok(_) => {
                    state.message = t!("msg-exported", "path" => path.display());
                    refresh(state, lib);
                }
                Err(e) => state.dialogs.error = Some(t!("msg-component-save-failed", "error" => e.message)),
            }
        }
    });
}

// --- Plugin API 3 Catalog UI ---

fn plugin_catalog_sidebar(ui: &mut Ui, state: &mut AppState, lib: &mut Library, idx: usize) {
    let full = vec2(ui.available_width(), 0.0);
    let Some(cat) = state.plugins.catalogs.get(idx).cloned() else { return };
    ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
        if ui.add(egui::Button::new(t!("lib-refresh")).min_size(full)).clicked() {
            state.plugin_catalog.reset();
        }
        if let crate::plugin_catalog::LoadState::Ready { notice, .. } = &state.plugin_catalog.state
            && !notice.is_empty()
        {
            ui.add_space(4.0);
            ui.label(theme::muted(notice).size(10.5));
        } else if !cat.description.is_empty() {
            ui.add_space(4.0);
            ui.label(theme::muted(&cat.description).size(10.5));
        }
        ui.add_space(6.0);
        ui.separator();
        ui.with_layout(Layout::top_down(Align::Min), |ui| {
            egui::ScrollArea::vertical().id_salt("lib-plugin-filters").auto_shrink([false, false]).show(ui, |ui| {
                if !state.prefs.plugins_network.contains(&cat.plugin_id) {
                    ui.label(RichText::new(t!("lib-catalog-network-off")).color(PALETTE.warning).size(11.5));
                    ui.add_space(6.0);
                }
                let categories = state.plugin_catalog.categories.clone();
                if !categories.is_empty() {
                    section_title(ui, t!("lib-category"));
                    let total = match state.plugin_catalog.state {
                        crate::plugin_catalog::LoadState::Ready { total, .. } => total,
                        _ => 0,
                    };
                    if filter_row(ui, &t!("lib-all-kinds"), total, lib.catalog_category.is_none()).clicked() {
                        lib.catalog_category = None;
                    }
                    for c in &categories {
                        let sel = lib.catalog_category.as_deref() == Some(c.id.as_str());
                        if filter_row(ui, &c.name, c.count, sel).clicked() {
                            lib.catalog_category = Some(c.id.clone());
                        }
                    }
                }
                let colors = state.plugin_catalog.colors.clone();
                if !colors.is_empty() {
                    ui.add_space(8.0);
                    section_title(ui, t!("lib-filter-color"));
                    let selected_color = lib.catalog_color.clone().unwrap_or_else(|| t!("lib-all-kinds"));
                    egui::ComboBox::from_id_salt("lib-plugin-color")
                        .selected_text(selected_color)
                        .width(ui.available_width().min(176.0))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut lib.catalog_color, None, t!("lib-all-kinds"));
                            for c in &colors {
                                ui.selectable_value(&mut lib.catalog_color, Some(c.clone()), c);
                            }
                        });
                }
                ui.add_space(8.0);
                section_title(ui, t!("lib-filter-max-dims"));
                for (label, buf) in [
                    (t!("lib-dim-w"), &mut lib.catalog_max_w),
                    (t!("lib-dim-d"), &mut lib.catalog_max_d),
                    (t!("lib-dim-h"), &mut lib.catalog_max_h),
                ] {
                    ui.horizontal(|ui| {
                        ui.label(theme::muted(label).size(11.0));
                        ui.add(egui::TextEdit::singleline(buf).desired_width(110.0));
                    });
                }
            });
        });
    });
}

fn plugin_catalog_grid(ui: &mut Ui, state: &mut AppState, lib: &mut Library, open_now: &mut bool) {
    match &state.plugin_catalog.state {
        crate::plugin_catalog::LoadState::Idle | crate::plugin_catalog::LoadState::Loading => {
            centered(ui, |ui| {
                ui.spinner();
                ui.label(theme::muted(t!("lib-catalog-loading")));
            });
            return;
        }
        crate::plugin_catalog::LoadState::Failed(msg) => {
            let msg = msg.clone();
            centered(ui, |ui| {
                ui.label(RichText::new(&msg).color(PALETTE.danger));
                if ui.button(t!("lib-try-again")).clicked() {
                    state.plugin_catalog.reset();
                }
            });
            return;
        }
        crate::plugin_catalog::LoadState::Ready { .. } => {}
    }

    if let Some(err) = state.plugin_catalog.search_error.clone() {
        ui.label(RichText::new(err).color(PALETTE.danger).size(11.5));
    }
    let items = state.plugin_catalog.items.clone();
    let total = state.plugin_catalog.total_matches;
    ui.label(theme::muted(format!("{total}")).size(11.5));
    ui.add_space(4.0);
    if items.is_empty() {
        centered(ui, |ui| {
            ui.label(theme::muted(t!("lib-none")));
        });
        return;
    }

    let width = ui.available_width();
    let cols = ((width + GAP) / (CARD + GAP)).floor().max(1.0) as usize;
    let card_w = ((width - GAP * (cols.saturating_sub(1)) as f32) / cols as f32).max(CARD * 0.75);
    let pic_h = card_w * 2.0 / 3.0;
    let card_h = pic_h + CARD_TEXT;
    let rows = items.len().div_ceil(cols);

    egui::ScrollArea::vertical().id_salt("lib-plugin-grid").auto_shrink([false, false]).show_rows(
        ui,
        card_h + GAP,
        rows,
        |ui, range| {
            for r in range {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = GAP;
                    for c in 0..cols {
                        let Some(item) = items.get(r * cols + c) else { break };
                        let selected = lib.catalog_selected.as_deref() == Some(item.id.as_str());
                        let (rect, response) = ui.allocate_exact_size(vec2(card_w, card_h), Sense::click());
                        let pic_rect = Rect::from_min_size(rect.min, vec2(card_w, pic_h));
                        ui.painter().rect_filled(rect, CARD_RADIUS, PALETTE.canvas);
                        if item.has_thumbnail {
                            state.plugin_catalog.want_thumbnail(&item.id);
                        }
                        let thumb = state.plugin_catalog.thumbnail(&item.id).cloned();
                        let corners = CornerRadius { nw: CARD_RADIUS - 1, ne: CARD_RADIUS - 1, sw: 0, se: 0 };
                        picture(ui, state, lib, pic_rect.shrink(1.0), thumb.as_deref(), None, corners);
                        let stroke = if selected {
                            Stroke::new(2.0, PALETTE.accent)
                        } else if response.hovered() {
                            Stroke::new(1.0, PALETTE.text_muted)
                        } else {
                            Stroke::new(1.0, PALETTE.border)
                        };
                        ui.painter().rect_stroke(rect, CARD_RADIUS, stroke, StrokeKind::Inside);
                        let text_rect = Rect::from_min_max(
                            Pos2::new(rect.left() + 6.0, pic_rect.bottom() + 4.0),
                            Pos2::new(rect.right() - 6.0, rect.bottom() - 4.0),
                        );
                        ui.scope_builder(egui::UiBuilder::new().max_rect(text_rect), |ui| {
                            ui.add(egui::Label::new(RichText::new(&item.name).strong().size(11.5)).truncate());
                            ui.add(egui::Label::new(theme::muted(&item.subtitle).size(10.5)).truncate());
                        });
                        if response.clicked() {
                            lib.catalog_selected = Some(item.id.clone());
                        }
                        if response.double_clicked() {
                            lib.catalog_selected = Some(item.id.clone());
                            *open_now = true;
                        }
                    }
                });
                ui.add_space(GAP);
            }
        },
    );
}

fn plugin_catalog_details(ui: &mut Ui, state: &mut AppState, lib: &mut Library) {
    let Some(id) = lib.catalog_selected.clone() else {
        centered(ui, |ui| {
            ui.label(theme::muted(t!("lib-pick")));
        });
        return;
    };
    let Some(item) = state.plugin_catalog.items.iter().find(|i| i.id == id).cloned() else { return };
    egui::ScrollArea::vertical().id_salt("lib-plugin-details").auto_shrink([false, false]).show(ui, |ui| {
        let width = ui.available_width();
        let (rect, _) = ui.allocate_exact_size(vec2(width, width * 2.0 / 3.0), Sense::hover());
        if item.has_thumbnail {
            state.plugin_catalog.want_thumbnail(&item.id);
        }
        let thumb = state.plugin_catalog.thumbnail(&item.id).cloned();
        picture(ui, state, lib, rect, thumb.as_deref(), None, CornerRadius::same(4));
        ui.painter().rect_stroke(rect, 4.0, Stroke::new(1.0, PALETTE.border), StrokeKind::Inside);
        ui.add_space(6.0);
        ui.add(egui::Label::new(theme::title(&item.name)).wrap());
        if !item.subtitle.is_empty() {
            ui.add(egui::Label::new(theme::muted(&item.subtitle)).wrap());
        }
        if !item.article.is_empty() {
            ui.add_space(4.0);
            ui.label(RichText::new(&item.article).monospace().size(11.5));
        }
        if !item.kind.is_empty() {
            ui.label(theme::muted(kind_label(&item.kind)).size(11.0));
        }
        if !item.colors.is_empty() {
            ui.label(theme::muted(item.colors.join(", ")).size(11.0));
        }
        let dims: Vec<String> = [
            item.width.map(|v| format!("W {}", cc_views::format::meters(v))),
            item.depth.map(|v| format!("D {}", cc_views::format::meters(v))),
            item.height.map(|v| format!("H {}", cc_views::format::meters(v))),
        ]
        .into_iter()
        .flatten()
        .collect();
        if !dims.is_empty() {
            ui.add_space(4.0);
            ui.label(theme::muted(dims.join(" × ")).size(11.5));
        }
        if item.bytes > 0 {
            ui.label(theme::muted(format!("{:.0} KB", item.bytes as f64 / 1024.0)).size(11.0));
        }
        if item.url.starts_with("https://") {
            ui.add_space(8.0);
            if ui.button(t!("lib-open-website")).clicked() {
                ui.ctx().open_url(egui::OpenUrl::new_tab(&item.url));
            }
        }
        if state.plugin_catalog.preparing.as_deref() == Some(item.id.as_str()) {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(theme::muted(t!("lib-catalog-loading")));
            });
        }
        if let Some(err) = &state.plugin_catalog.prepare_error {
            ui.add_space(6.0);
            ui.label(RichText::new(err).color(PALETTE.danger).size(11.5));
        }
    });
}
