//! Background worker and state for Plugin API 3 Component Library catalogs
//! (`[[catalogs]]` in `plugin.toml`, `ccad.catalog { load, search, thumbnail, prepare }`).
//!
//! Because `mlua::Lua` is `!Send`, each active catalog source runs its plugin
//! inside a dedicated worker thread communicating over `std::sync::mpsc`.
//! The UI thread never blocks on network or Lua work.

use cc_plugins::{Limits, Permissions, Plugin, store};
use egui::Context;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

/// A category option returned by `load()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogCategory {
    pub id: String,
    pub name: String,
    pub count: usize,
}

/// One catalog item returned by `search(query)`.
#[derive(Debug, Clone, Default)]
pub struct CatalogItem {
    pub id: String,
    pub name: String,
    pub subtitle: String,
    pub kind: String,
    pub article: String,
    pub bytes: u64,
    pub width: Option<f64>,
    pub depth: Option<f64>,
    pub height: Option<f64>,
    pub colors: Vec<String>,
    pub url: String,
    pub has_thumbnail: bool,
}

/// Active query parameters sent to the plugin's `search(query)`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CatalogQuery {
    pub text: String,
    pub category: Option<String>,
    pub color: Option<String>,
    pub max_width: Option<f64>,
    pub max_depth: Option<f64>,
    pub max_height: Option<f64>,
}

/// Result of `prepare(item_id)`: an engine component-load command and its args.
#[derive(Debug, Clone)]
pub struct PreparedLoad {
    pub command: String,
    pub args: Value,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum LoadState {
    #[default]
    Idle,
    Loading,
    Ready {
        total: usize,
        notice: String,
    },
    Failed(String),
}

/// Page size fetched per search request; additional pages load on demand.
const PAGE_SIZE: usize = 60;
/// Maximum GPU thumbnail textures kept in memory per catalog session.
const MAX_THUMBNAIL_TEXTURES: usize = 192;

enum WorkerReq {
    Load,
    Search(CatalogQuery, usize),
    Thumbnail(String),
    Prepare(String),
}

enum WorkerResp {
    Loaded(Result<(usize, String, Vec<CatalogCategory>, Vec<String>), String>),
    Searched(CatalogQuery, usize, Result<(usize, Vec<CatalogItem>), String>),
    Thumbnail(String, Result<Option<egui::ColorImage>, String>),
    Prepared(String, Result<PreparedLoad, String>),
}

/// State of one active plugin catalog source in `AppState`.
#[derive(Default)]
pub struct PluginCatalogClient {
    active: Option<(String, String)>,
    tx: Option<Sender<WorkerReq>>,
    rx: Option<Receiver<WorkerResp>>,
    pub state: LoadState,
    pub categories: Arc<[CatalogCategory]>,
    pub colors: Arc<[String]>,
    pub items: Arc<[CatalogItem]>,
    pub total_matches: usize,
    pub loading_more: bool,
    pub search_error: Option<String>,
    last_query: Option<CatalogQuery>,
    thumbnails: BTreeMap<String, egui::TextureHandle>,
    thumbnail_lru: std::collections::VecDeque<String>,
    thumbnails_pending: BTreeSet<String>,
    thumbnails_failed: BTreeSet<String>,
    pub preparing: Option<String>,
    pub prepare_error: Option<String>,
}

impl std::fmt::Debug for PluginCatalogClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginCatalogClient")
            .field("active", &self.active)
            .field("state", &self.state)
            .field("total_matches", &self.total_matches)
            .field("loaded_items", &self.items.len())
            .field("cached_textures", &self.thumbnails.len())
            .finish()
    }
}

impl PluginCatalogClient {
    /// Switch to `(plugin_id, catalog_id)`, starting its worker thread on demand.
    pub fn activate(
        &mut self,
        ctx: &Context,
        plugin_id: &str,
        catalog_id: &str,
        plugin_dir: &Path,
        plugins_folder: Option<&Path>,
        network_granted: bool,
    ) {
        let key = (plugin_id.to_string(), catalog_id.to_string());
        if self.active.as_ref() == Some(&key) {
            return;
        }
        self.reset();
        self.active = Some(key);
        if cfg!(test) {
            self.state = LoadState::Loading;
            return;
        }
        let (req_tx, req_rx) = mpsc::channel();
        let (resp_tx, resp_rx) = mpsc::channel();
        let dir = plugin_dir.to_path_buf();
        let folder = plugins_folder.map(Path::to_path_buf);
        let pid = plugin_id.to_string();
        let cid = catalog_id.to_string();
        let cache_root = crate::recovery::data_dir()
            .map(|d| d.join("plugin-cache").join(&pid))
            .unwrap_or_else(|| dir.join(".cache"));
        let repaint = ctx.clone();

        let _ = std::thread::Builder::new().name(format!("plugin-catalog-{pid}")).spawn(move || {
            let stored = folder.as_deref().map(|f| store::read_settings(f, &pid)).unwrap_or_default();
            let mut plugin = match Plugin::load_with_settings(&dir, Limits::default(), &stored) {
                Ok(p) => p,
                Err(e) => {
                    let _ = resp_tx.send(WorkerResp::Loaded(Err(e.message)));
                    repaint.request_repaint();
                    return;
                }
            };
            plugin.set_cache_root(cache_root.clone());
            plugin.grant(Permissions { network: network_granted, ..Permissions::NONE });

            let mut load_pending = false;
            let mut prepare_queue = std::collections::VecDeque::new();
            let mut latest_search: Option<(CatalogQuery, usize)> = None;
            let mut thumb_queue = std::collections::VecDeque::new();
            let mut disconnected = false;

            let enqueue = |req: WorkerReq,
                           load_pending: &mut bool,
                           prepare_queue: &mut std::collections::VecDeque<String>,
                           latest_search: &mut Option<(CatalogQuery, usize)>,
                           thumb_queue: &mut std::collections::VecDeque<String>| {
                match req {
                    WorkerReq::Load => *load_pending = true,
                    WorkerReq::Prepare(id) => prepare_queue.push_back(id),
                    WorkerReq::Search(q, offset) => *latest_search = Some((q, offset)),
                    WorkerReq::Thumbnail(id) => thumb_queue.push_back(id),
                }
            };

            loop {
                // Drain all currently queued requests so higher-priority work jumps ahead
                // and stale keystroke searches coalesce into the latest query.
                loop {
                    match req_rx.try_recv() {
                        Ok(req) => {
                            enqueue(req, &mut load_pending, &mut prepare_queue, &mut latest_search, &mut thumb_queue)
                        }
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => {
                            disconnected = true;
                            break;
                        }
                    }
                }
                let next = if load_pending {
                    load_pending = false;
                    Some(WorkerReq::Load)
                } else if let Some(id) = prepare_queue.pop_front() {
                    Some(WorkerReq::Prepare(id))
                } else if let Some((q, offset)) = latest_search.take() {
                    Some(WorkerReq::Search(q, offset))
                } else if let Some(id) = thumb_queue.pop_front() {
                    Some(WorkerReq::Thumbnail(id))
                } else if disconnected {
                    break;
                } else {
                    match req_rx.recv() {
                        Ok(req) => {
                            enqueue(req, &mut load_pending, &mut prepare_queue, &mut latest_search, &mut thumb_queue);
                            continue;
                        }
                        Err(_) => break,
                    }
                };
                let Some(req) = next else { continue };
                let resp = match req {
                    WorkerReq::Load => WorkerResp::Loaded(run_load(&mut plugin, &cid)),
                    WorkerReq::Search(q, offset) => {
                        let res = run_search(&mut plugin, &cid, &q, offset);
                        WorkerResp::Searched(q, offset, res)
                    }
                    WorkerReq::Thumbnail(id) => {
                        let res = plugin
                            .catalog_thumbnail(&cid, &id)
                            .map_err(|e| e.message)
                            .map(|opt| opt.and_then(|p| decode_thumbnail_file(&p)));
                        WorkerResp::Thumbnail(id, res)
                    }
                    WorkerReq::Prepare(id) => {
                        let res = run_prepare(&mut plugin, &cid, &id, &cache_root);
                        WorkerResp::Prepared(id, res)
                    }
                };
                if resp_tx.send(resp).is_err() {
                    break;
                }
                repaint.request_repaint();
            }
        });

        let _ = req_tx.send(WorkerReq::Load);
        self.tx = Some(req_tx);
        self.rx = Some(resp_rx);
        self.state = LoadState::Loading;
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Poll worker responses, trigger search when `query` changes, upload
    /// worker-decoded thumbnails to GPU textures, and return a [`PreparedLoad`]
    /// if a requested item just finished downloading.
    pub fn sync(&mut self, ctx: &Context, query: &CatalogQuery) -> Option<PreparedLoad> {
        let mut prepared = None;
        if let Some(rx) = &self.rx {
            loop {
                match rx.try_recv() {
                    Ok(WorkerResp::Loaded(Ok((total, notice, categories, colors)))) => {
                        self.state = LoadState::Ready { total, notice };
                        self.categories = categories.into();
                        self.colors = colors.into();
                        self.last_query = None;
                        self.loading_more = false;
                    }
                    Ok(WorkerResp::Loaded(Err(msg))) => {
                        self.state = LoadState::Failed(msg);
                    }
                    Ok(WorkerResp::Searched(q, offset, Ok((total, page)))) => {
                        if self.last_query.as_ref() == Some(&q) {
                            self.total_matches = total;
                            self.loading_more = false;
                            if offset == 0 {
                                self.items = page.into();
                            } else if offset == self.items.len() {
                                let mut combined = Vec::with_capacity(self.items.len() + page.len());
                                combined.extend_from_slice(&self.items);
                                combined.extend(page);
                                self.items = combined.into();
                            }
                            self.search_error = None;
                        }
                    }
                    Ok(WorkerResp::Searched(_, _, Err(msg))) => {
                        self.loading_more = false;
                        self.search_error = Some(msg);
                    }
                    Ok(WorkerResp::Thumbnail(id, Ok(Some(image)))) => {
                        self.thumbnails_pending.remove(&id);
                        let tex = ctx.load_texture(format!("catalog-thumb-{id}"), image, egui::TextureOptions::LINEAR);
                        if self.thumbnails.insert(id.clone(), tex).is_none() {
                            self.thumbnail_lru.push_back(id);
                            while self.thumbnails.len() > MAX_THUMBNAIL_TEXTURES {
                                if let Some(oldest) = self.thumbnail_lru.pop_front() {
                                    self.thumbnails.remove(&oldest);
                                } else {
                                    break;
                                }
                            }
                        }
                    }
                    Ok(WorkerResp::Thumbnail(id, Ok(None) | Err(_))) => {
                        self.thumbnails_pending.remove(&id);
                        self.thumbnails_failed.insert(id);
                    }
                    Ok(WorkerResp::Prepared(id, Ok(load))) => {
                        if self.preparing.as_deref() == Some(&id) {
                            self.preparing = None;
                            self.prepare_error = None;
                            prepared = Some(load);
                        }
                    }
                    Ok(WorkerResp::Prepared(id, Err(msg))) => {
                        if self.preparing.as_deref() == Some(&id) {
                            self.preparing = None;
                            self.prepare_error = Some(msg);
                        }
                    }
                    Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
                }
            }
        }
        if matches!(self.state, LoadState::Ready { .. }) && self.last_query.as_ref() != Some(query) {
            self.last_query = Some(query.clone());
            self.loading_more = false;
            if let Some(tx) = &self.tx {
                let _ = tx.send(WorkerReq::Search(query.clone(), 0));
            }
        }
        prepared
    }

    /// Request the next page of results for the active query.
    pub fn load_more(&mut self) {
        if self.loading_more || self.items.len() >= self.total_matches {
            return;
        }
        if let (Some(tx), Some(q)) = (&self.tx, &self.last_query) {
            self.loading_more = true;
            let _ = tx.send(WorkerReq::Search(q.clone(), self.items.len()));
        }
    }

    pub fn thumbnail(&self, item_id: &str) -> Option<&egui::TextureHandle> {
        self.thumbnails.get(item_id)
    }

    pub fn want_thumbnail(&mut self, item_id: &str) {
        if self.thumbnails.contains_key(item_id)
            || self.thumbnails_pending.contains(item_id)
            || self.thumbnails_failed.contains(item_id)
            || self.thumbnails_pending.len() >= 4
        {
            return;
        }
        if let Some(tx) = &self.tx {
            self.thumbnails_pending.insert(item_id.to_string());
            let _ = tx.send(WorkerReq::Thumbnail(item_id.to_string()));
        }
    }

    pub fn request_prepare(&mut self, item_id: &str) {
        if self.preparing.as_deref() == Some(item_id) {
            return;
        }
        self.preparing = Some(item_id.to_string());
        self.prepare_error = None;
        if let Some(tx) = &self.tx {
            let _ = tx.send(WorkerReq::Prepare(item_id.to_string()));
        }
    }

    /// Inject loaded catalog state in UI tests without touching the network.
    #[cfg(test)]
    pub fn set_loaded(
        &mut self,
        plugin_id: &str,
        catalog_id: &str,
        categories: Vec<CatalogCategory>,
        colors: Vec<String>,
        items: Vec<CatalogItem>,
    ) {
        self.active = Some((plugin_id.to_string(), catalog_id.to_string()));
        self.total_matches = items.len();
        self.state = LoadState::Ready { total: items.len(), notice: String::new() };
        self.categories = categories.into();
        self.colors = colors.into();
        self.items = items.into();
        self.last_query = Some(CatalogQuery::default());
    }
}

fn decode_thumbnail_file(path: &Path) -> Option<egui::ColorImage> {
    let bytes = std::fs::read(path).ok()?;
    let decoded = image::load_from_memory(&bytes).ok()?.to_rgba8();
    let size = [decoded.width() as usize, decoded.height() as usize];
    Some(egui::ColorImage::from_rgba_unmultiplied(size, decoded.as_raw()))
}

fn run_load(plugin: &mut Plugin, cid: &str) -> Result<(usize, String, Vec<CatalogCategory>, Vec<String>), String> {
    let val = plugin.catalog_load(cid).map_err(|e| e.message)?;
    let total = val["total"].as_u64().unwrap_or(0) as usize;
    let notice = val["notice"].as_str().unwrap_or_default().to_string();
    let categories = val["categories"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| {
            Some(CatalogCategory {
                id: c.get("id")?.as_str()?.to_string(),
                name: c.get("name")?.as_str()?.to_string(),
                count: c.get("count").and_then(Value::as_u64).unwrap_or(0) as usize,
            })
        })
        .collect();
    let colors =
        val["colors"].as_array().into_iter().flatten().filter_map(|c| c.as_str().map(str::to_string)).collect();
    Ok((total, notice, categories, colors))
}

fn run_search(
    plugin: &mut Plugin,
    cid: &str,
    q: &CatalogQuery,
    offset: usize,
) -> Result<(usize, Vec<CatalogItem>), String> {
    let mut map = serde_json::Map::new();
    map.insert("text".into(), json!(q.text));
    if let Some(category) = &q.category {
        map.insert("category".into(), json!(category));
    }
    if let Some(color) = &q.color {
        map.insert("color".into(), json!(color));
    }
    if let Some(w) = q.max_width {
        map.insert("max_width".into(), json!(w));
    }
    if let Some(d) = q.max_depth {
        map.insert("max_depth".into(), json!(d));
    }
    if let Some(h) = q.max_height {
        map.insert("max_height".into(), json!(h));
    }
    map.insert("offset".into(), json!(offset));
    map.insert("limit".into(), json!(PAGE_SIZE));
    let payload = Value::Object(map);
    let val = plugin.catalog_search(cid, payload).map_err(|e| e.message)?;
    let total = val["total"].as_u64().unwrap_or(0) as usize;
    let items = val["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|it| {
            let dims = it.get("dimensions");
            Some(CatalogItem {
                id: it.get("id")?.as_str()?.to_string(),
                name: it.get("name")?.as_str()?.to_string(),
                subtitle: it.get("subtitle").and_then(Value::as_str).unwrap_or_default().to_string(),
                kind: it.get("kind").and_then(Value::as_str).unwrap_or("generic").to_string(),
                article: it.get("article").and_then(Value::as_str).unwrap_or_default().to_string(),
                bytes: it.get("bytes").and_then(Value::as_u64).unwrap_or(0),
                width: dims.and_then(|d| d.get("width")).and_then(Value::as_f64),
                depth: dims.and_then(|d| d.get("depth")).and_then(Value::as_f64),
                height: dims.and_then(|d| d.get("height")).and_then(Value::as_f64),
                colors: it
                    .get("colors")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|c| c.as_str().map(str::to_string))
                    .collect(),
                url: it.get("url").and_then(Value::as_str).unwrap_or_default().to_string(),
                has_thumbnail: it.get("has_thumbnail").and_then(Value::as_bool).unwrap_or(false),
            })
        })
        .collect();
    Ok((total, items))
}

fn run_prepare(plugin: &mut Plugin, cid: &str, item_id: &str, cache_root: &Path) -> Result<PreparedLoad, String> {
    let val = plugin.catalog_prepare(cid, item_id).map_err(|e| e.message)?;
    let command = val
        .get("command")
        .and_then(Value::as_str)
        .filter(|c| matches!(*c, "gltf_component_load" | "component_load" | "skp_component_load"))
        .ok_or_else(|| "catalog prepare must return a component load command".to_string())?
        .to_string();
    let args = val.get("args").cloned().unwrap_or(Value::Null);
    let path_str = args.get("path").and_then(Value::as_str).ok_or_else(|| "missing args.path".to_string())?;
    let path = std::fs::canonicalize(path_str).map_err(|e| format!("cached file missing: {e}"))?;
    let root = std::fs::canonicalize(cache_root).map_err(|e| format!("cache root missing: {e}"))?;
    if !path.starts_with(&root) {
        return Err("prepared file must reside inside the plugin's cache folder".into());
    }
    Ok(PreparedLoad { command, args, path })
}
