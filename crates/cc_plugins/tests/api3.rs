//! Integration tests for Plugin API 3: `permissions = ["network"]`,
//! `allowed_hosts`, `ccad.http`, and `[[catalogs]]` providers.

use cc_plugins::{Limits, Permissions, Plugin};
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn temp_plugin(name: &str, toml: &str, lua: &str) -> PathBuf {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("ccad-api3-{name}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("plugin.toml"), toml).unwrap();
    std::fs::write(dir.join("main.lua"), lua).unwrap();
    dir
}

#[test]
fn network_permission_requires_api3_and_allowed_hosts() {
    let dir_v2 = temp_plugin(
        "net_v2",
        r#"
id = "net_v2"
name = "Net v2"
version = "0.1.0"
api = 2
permissions = ["network"]
allowed_hosts = ["huggingface.co"]
"#,
        "",
    );
    assert!(Plugin::load(&dir_v2, Limits::default()).is_err());

    let dir_no_hosts = temp_plugin(
        "net_no_hosts",
        r#"
id = "net_no_hosts"
name = "Net no hosts"
version = "0.1.0"
api = 3
permissions = ["network"]
"#,
        "",
    );
    assert!(Plugin::load(&dir_no_hosts, Limits::default()).is_err());

    let dir_bad_host = temp_plugin(
        "net_bad_host",
        r#"
id = "net_bad_host"
name = "Net bad host"
version = "0.1.0"
api = 3
permissions = ["network"]
allowed_hosts = ["localhost"]
"#,
        "",
    );
    assert!(Plugin::load(&dir_bad_host, Limits::default()).is_err());
}

#[test]
fn catalog_provider_loads_searches_and_enforces_network_permission() {
    let dir = temp_plugin(
        "cat_ok",
        r#"
id = "cat_ok"
name = "Catalog Test"
version = "0.1.0"
api = 3
permissions = ["network"]
allowed_hosts = ["huggingface.co", "*.hf.co"]

[[catalogs]]
id = "furniture"
label = "Furniture"
description = "Sample catalog"
"#,
        r#"
ccad.catalog {
    id = "furniture",
    load = function()
        return {
            total = 1,
            categories = ccad.array({ { id = "st001", name = "Storage", count = 1 } }),
            colors = ccad.array({ "white" }),
            notice = "Test notice",
        }
    end,
    search = function(q)
        local text = (q.text or ""):lower()
        local items = ccad.array()
        if text == "" or ("omar"):find(text, 1, true) then
            items[1] = { id = "00069768", name = "OMAR", subtitle = "Shelf unit", kind = "casework" }
        end
        return { total = #items, items = items }
    end,
    thumbnail = function(id)
        return ccad.http.cached("thumbs/" .. id .. ".jpg")
    end,
    prepare = function(id)
        -- Attempting network fetch without network permission must fail.
        if id == "net_test" then
            return ccad.http.get("https://huggingface.co/datasets/T-K-233/ikea-furniture/resolve/main/data/manifest.json")
        end
        return { command = "gltf_component_load", args = { path = "dummy.glb" } }
    end,
}
"#,
    );

    let mut plugin = Plugin::load(&dir, Limits::default()).unwrap();
    assert_eq!(plugin.catalogs().len(), 1);
    assert_eq!(plugin.catalogs()[0].label, "Furniture");

    let loaded = plugin.catalog_load("furniture").unwrap();
    assert_eq!(loaded["total"], json!(1));
    assert_eq!(loaded["notice"], json!("Test notice"));

    let found = plugin.catalog_search("furniture", json!({ "text": "om" })).unwrap();
    assert_eq!(found["total"], json!(1));
    assert_eq!(found["items"][0]["name"], json!("OMAR"));

    let none = plugin.catalog_search("furniture", json!({ "text": "zzz" })).unwrap();
    assert_eq!(none["total"], json!(0));

    // Cached lookup works without network and returns None when absent.
    assert_eq!(plugin.catalog_thumbnail("furniture", "00069768").unwrap(), None);

    // Without network permission granted, ccad.http.get is refused.
    assert_eq!(plugin.granted(), Permissions::NONE);
    let err = plugin.catalog_prepare("furniture", "net_test").unwrap_err();
    assert!(err.message.contains("network permission"), "unexpected error: {}", err.message);
}

#[test]
fn read_cached_reads_valid_cache_and_evicts_sha256_mismatch() {
    let dir = temp_plugin(
        "cat_cache",
        r#"
id = "cat_cache"
name = "Cache Test"
version = "0.1.0"
api = 3
permissions = ["network"]
allowed_hosts = ["huggingface.co"]

[[catalogs]]
id = "c"
label = "C"
"#,
        r#"
ccad.catalog {
    id = "c",
    load = function()
        local raw = ccad.http.read_cached("data/manifest.json")
        return { raw = raw }
    end,
    search = function(q)
        local raw = ccad.http.read_cached("data/manifest.json", { sha256 = q.sha256 })
        return { found = raw ~= nil }
    end,
    prepare = function(_) return {} end,
}
"#,
    );

    let mut plugin = Plugin::load(&dir, Limits::default()).unwrap();
    let cache = plugin.cache_root().join("data");
    std::fs::create_dir_all(&cache).unwrap();
    let payload = br#"{"version":1}"#;
    let file = cache.join("manifest.json");
    std::fs::write(&file, payload).unwrap();

    // Reads cached JSON without network permission.
    let loaded = plugin.catalog_load("c").unwrap();
    assert_eq!(loaded["raw"], json!(r#"{"version":1}"#));

    // Correct SHA-256 passes.
    let good = cc_plugins::http::sha256_hex(payload);
    let ok = plugin.catalog_search("c", json!({ "sha256": good })).unwrap();
    assert_eq!(ok["found"], json!(true));

    // Wrong SHA-256 returns nil and removes the corrupted cache file.
    let bad = "0".repeat(64);
    let miss = plugin.catalog_search("c", json!({ "sha256": bad })).unwrap();
    assert_eq!(miss["found"], json!(false));
    assert!(!file.exists(), "corrupted cache file should be evicted");
}
