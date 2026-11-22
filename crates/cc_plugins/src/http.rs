//! Scoped HTTPS client and per-plugin file cache for Plugin API 3
//! (`permissions = ["network"]` and `allowed_hosts`).
//!
//! Every request is restricted to `https://`, checked against the plugin's
//! declared `allowed_hosts` on the initial URL and on every redirect, bounded
//! in response size and time, and optionally verified against an expected
//! SHA-256 digest before writing into the plugin's isolated cache folder.

use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Most host patterns a plugin manifest may declare in `allowed_hosts`.
pub const MAX_HOSTS: usize = 16;
/// Hard ceiling on a single HTTP response or cached download (64 MiB).
pub const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
/// Default size cap for `ccad.http.get` (16 MiB).
pub const DEFAULT_GET_BYTES: usize = 16 * 1024 * 1024;
/// Default and maximum request timeout, in seconds.
pub const DEFAULT_TIMEOUT_SECS: u64 = 30;
pub const MAX_TIMEOUT_SECS: u64 = 120;
/// Maximum HTTP redirects followed per request.
const MAX_REDIRECTS: usize = 5;
/// Maximum length and depth of a plugin cache key.
const MAX_CACHE_KEY_LEN: usize = 160;
const MAX_CACHE_KEY_DEPTH: usize = 5;

/// Validate an `allowed_hosts` entry from `plugin.toml`.
///
/// Accepts lowercase ASCII hostnames (e.g. `huggingface.co`) or single-prefix
/// wildcards (`*.hf.co`), with at least one dot and no scheme, port, path,
/// whitespace, `localhost`, or IP-literal-only labels.
pub fn valid_host(host: &str) -> bool {
    let bare = host.strip_prefix("*.").unwrap_or(host);
    if bare.len() < 3 || bare.len() > 253 || !bare.contains('.') {
        return false;
    }
    if bare.eq_ignore_ascii_case("localhost") || bare.ends_with(".localhost") {
        return false;
    }
    let mut all_digits = true;
    for label in bare.split('.') {
        if label.is_empty() || label.len() > 63 {
            return false;
        }
        let bytes = label.as_bytes();
        if bytes.first() == Some(&b'-') || bytes.last() == Some(&b'-') {
            return false;
        }
        if !bytes.iter().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-') {
            return false;
        }
        if !bytes.iter().all(|b| b.is_ascii_digit()) {
            all_digits = false;
        }
    }
    !all_digits
}

/// Extract the lowercase hostname from an `https://` URL, rejecting non-HTTPS
/// schemes, userinfo (`@`), explicit ports, whitespace, or control characters.
pub fn extract_https_host(url: &str) -> Result<String, String> {
    let Some(rest) = url.strip_prefix("https://") else {
        return Err("only https:// URLs are allowed".into());
    };
    if rest.is_empty() || url.bytes().any(|b| b.is_ascii_control() || b == b' ') {
        return Err("invalid URL".into());
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') || authority.contains(':') {
        return Err("URL must have a plain HTTPS hostname without credentials or port".into());
    }
    let host = authority.to_ascii_lowercase();
    if !valid_host(&host) {
        return Err(format!("invalid host \"{host}\""));
    }
    Ok(host)
}

/// Check whether `url` is `https://` and its hostname matches one of `allowed_hosts`.
pub fn check_url_allowed(url: &str, allowed_hosts: &[String]) -> Result<(), String> {
    let host = extract_https_host(url)?;
    let matched = allowed_hosts.iter().any(|pattern| {
        if let Some(suffix) = pattern.strip_prefix("*.") {
            host.len() > suffix.len() + 1
                && host.ends_with(suffix)
                && host.as_bytes()[host.len() - suffix.len() - 1] == b'.'
        } else {
            host == *pattern
        }
    });
    if !matched {
        return Err(format!("host \"{host}\" is not in allowed_hosts"));
    }
    Ok(())
}

/// Validate a relative plugin cache key (e.g. `data/manifest.json` or
/// `models/<sha256>.glb`) so it can never escape the plugin's cache folder.
pub fn validate_cache_key(key: &str) -> Result<PathBuf, String> {
    if key.is_empty() || key.len() > MAX_CACHE_KEY_LEN {
        return Err("cache key must be 1 to 160 characters".into());
    }
    if key.starts_with('/') || key.ends_with('/') || key.contains('\\') {
        return Err("cache key must be a relative slash-separated path".into());
    }
    let mut out = PathBuf::new();
    let mut depth = 0usize;
    for part in key.split('/') {
        depth += 1;
        if depth > MAX_CACHE_KEY_DEPTH {
            return Err("cache key is too deep".into());
        }
        if part.is_empty()
            || part == "."
            || part == ".."
            || !part.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        {
            return Err(format!("invalid cache key segment \"{part}\""));
        }
        out.push(part);
    }
    Ok(out)
}

/// Compute lowercase hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut hex, "{b:02x}");
    }
    hex
}

/// Validate a 64-character lowercase/uppercase hex SHA-256 string.
pub fn normalize_sha256(hash: &str) -> Result<String, String> {
    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("sha256 must be 64 hexadecimal characters".into());
    }
    Ok(hash.to_ascii_lowercase())
}

/// Return the path of an already-cached file if it exists (and matches
/// `expected_sha256`, when given). Does not touch the network.
pub fn cached_file(
    cache_root: &Path,
    cache_key: &str,
    expected_sha256: Option<&str>,
) -> Result<Option<PathBuf>, String> {
    let rel = validate_cache_key(cache_key)?;
    let expected = expected_sha256.map(normalize_sha256).transpose()?;
    let path = cache_root.join(rel);
    if !path.is_file() {
        return Ok(None);
    }
    if let Some(expected) = expected {
        let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
        if sha256_hex(&bytes) != expected {
            let _ = std::fs::remove_file(&path);
            return Ok(None);
        }
    }
    Ok(Some(path))
}

/// Fetch `url` over HTTPS, checking `allowed_hosts` on the initial request and
/// on every redirect, capped at `max_bytes` and `timeout_secs`.
pub fn fetch_bytes(
    url: &str,
    allowed_hosts: &[String],
    max_bytes: usize,
    timeout_secs: u64,
) -> Result<(Vec<u8>, u16), String> {
    let limit = max_bytes.min(MAX_RESPONSE_BYTES);
    let timeout = Duration::from_secs(timeout_secs.clamp(1, MAX_TIMEOUT_SECS));
    check_url_allowed(url, allowed_hosts)?;

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .into();

    let mut current = url.to_string();
    for _ in 0..=MAX_REDIRECTS {
        check_url_allowed(&current, allowed_hosts)?;
        let response = agent
            .get(&current)
            .header("User-Agent", concat!("ConstructCraft/", env!("CARGO_PKG_VERSION")))
            .call()
            .map_err(|e| format!("http request failed: {e}"))?;
        let status = response.status().as_u16();
        if matches!(status, 301 | 302 | 303 | 307 | 308) {
            let Some(loc) = response.headers().get("location").and_then(|v| v.to_str().ok()) else {
                return Err(format!("HTTP {status} redirect without Location header"));
            };
            current = resolve_redirect(&current, loc)?;
            continue;
        }
        if !(200..300).contains(&status) {
            return Err(format!("HTTP {status} for {current}"));
        }
        let mut reader = response.into_body().into_reader().take(limit as u64 + 1);
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).map_err(|e| format!("failed reading response: {e}"))?;
        if buf.len() > limit {
            return Err(format!("response exceeds limit of {limit} bytes"));
        }
        return Ok((buf, status));
    }
    Err("too many HTTP redirects".into())
}

/// Resolve an absolute or root-relative redirect `location` against `base`.
fn resolve_redirect(base: &str, location: &str) -> Result<String, String> {
    if location.starts_with("https://") {
        return Ok(location.to_string());
    }
    if location.starts_with('/') {
        let host = extract_https_host(base)?;
        return Ok(format!("https://{host}{location}"));
    }
    Err("redirect target must be an https:// URL or root-relative path".into())
}

/// Download `url` into `cache_root/<cache_key>` (reusing an existing valid
/// cached file when present), verifying `expected_sha256` if provided.
pub fn download_cached(
    url: &str,
    allowed_hosts: &[String],
    cache_root: &Path,
    cache_key: &str,
    max_bytes: usize,
    timeout_secs: u64,
    expected_sha256: Option<&str>,
) -> Result<PathBuf, String> {
    let rel = validate_cache_key(cache_key)?;
    let expected = expected_sha256.map(normalize_sha256).transpose()?;
    if let Some(existing) = cached_file(cache_root, cache_key, expected.as_deref())? {
        return Ok(existing);
    }
    let (bytes, _) = fetch_bytes(url, allowed_hosts, max_bytes, timeout_secs)?;
    if let Some(expected) = expected {
        let actual = sha256_hex(&bytes);
        if actual != expected {
            return Err(format!("sha256 mismatch: expected {expected}, got {actual}"));
        }
    }
    let dest = cache_root.join(&rel);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create cache dir: {e}"))?;
    }
    let tmp = dest.with_extension("part");
    std::fs::write(&tmp, &bytes).map_err(|e| format!("write cache file: {e}"))?;
    std::fs::rename(&tmp, &dest).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("commit cache file: {e}")
    })?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_validation_accepts_domains_and_wildcards() {
        assert!(valid_host("huggingface.co"));
        assert!(valid_host("*.hf.co"));
        assert!(valid_host("cas-bridge.xethub.hf.co"));
        assert!(!valid_host("localhost"));
        assert!(!valid_host("127.0.0.1"));
        assert!(!valid_host("https://huggingface.co"));
        assert!(!valid_host("huggingface.co:443"));
        assert!(!valid_host("HuggingFace.co"));
        assert!(!valid_host("*.co"));
    }

    #[test]
    fn url_allowlist_checks_exact_and_wildcard_hosts() {
        let allowed = vec!["huggingface.co".into(), "*.hf.co".into()];
        assert!(check_url_allowed("https://huggingface.co/datasets/T-K-233/ikea-furniture", &allowed).is_ok());
        assert!(check_url_allowed("https://cas-bridge.xethub.hf.co/file", &allowed).is_ok());
        assert!(check_url_allowed("http://huggingface.co/file", &allowed).is_err());
        assert!(check_url_allowed("https://evilhuggingface.co/file", &allowed).is_err());
        assert!(check_url_allowed("https://hf.co/file", &allowed).is_err());
        assert!(check_url_allowed("https://user@huggingface.co/file", &allowed).is_err());
    }

    #[test]
    fn cache_key_prevents_traversal() {
        assert!(validate_cache_key("data/manifest.json").is_ok());
        assert!(validate_cache_key("models/ab/00069768.glb").is_ok());
        assert!(validate_cache_key("../escape.glb").is_err());
        assert!(validate_cache_key("/abs/path.glb").is_err());
        assert!(validate_cache_key("a\\b.glb").is_err());
        assert!(validate_cache_key("").is_err());
    }

    #[test]
    fn sha256_matches_known_vector() {
        assert_eq!(sha256_hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }
}
