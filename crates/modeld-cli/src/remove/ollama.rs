//! Removing Ollama models through Ollama's own API.
//!
//! Ollama's blobs are shared between tags, and its manifests say which tag
//! uses which blob, so files are never deleted behind its back: modeld asks
//! the running Ollama to delete every tag using the blob, and Ollama deletes
//! the blobs no remaining tag uses. Before that, `GET /api/tags` must list
//! each tag with the SHA-256 of the manifest file modeld read; otherwise the
//! running Ollama uses other folders than the ones modeld scanned.
//!
//! The API must be on this machine: `OLLAMA_HOST` must name a loopback or
//! unspecified address. The HTTP client is a few lines over `TcpStream`, as
//! the two calls need nothing more.

use super::guard::Guard;
use super::{Action, Collateral, Draft, RefusalCode, Removal, Target};
use modeld_core::Digest;
use modeld_providers::ollama::{ManifestInfo, manifest_scan};
use serde::Deserialize;
use std::collections::{BTreeMap, HashSet};
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// How long Ollama gets to list its tags; a running Ollama answers at once.
const TAGS_TIMEOUT: Duration = Duration::from_secs(1);

/// How long one delete may take; Ollama removes the blob files first.
const DELETE_TIMEOUT: Duration = Duration::from_secs(30);

const DEFAULT_PORT: u16 = 11434;

/// Where Ollama's API listens, from `OLLAMA_HOST`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Api {
    address: Result<SocketAddr, String>,
}

/// Why the API could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ApiError {
    NotLoopback(String),
    NotRunning(String),
}

/// The body of `GET /api/tags`.
#[derive(Debug, Deserialize)]
struct Listing {
    #[serde(default)]
    models: Vec<Tag>,
}

/// One tag `GET /api/tags` lists.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct Tag {
    name: String,
    /// SHA-256 of the tag's manifest file, in hex.
    digest: String,
}

impl Api {
    /// The API `OLLAMA_HOST` names, as the `ollama` CLI reads it.
    #[must_use]
    pub fn from_env() -> Self {
        Self::parse(std::env::var("OLLAMA_HOST").ok().as_deref())
    }

    /// Reads an `OLLAMA_HOST` value: `[http://]host[:port]`.
    ///
    /// Only loopback and unspecified addresses (which reach this machine)
    /// are accepted, and `localhost`; other names are not resolved.
    fn parse(value: Option<&str>) -> Self {
        let value = value.map(str::trim).unwrap_or_default();
        let without_scheme = value.strip_prefix("http://").unwrap_or(value);
        let address = if without_scheme.contains("://") {
            Err(format!("OLLAMA_HOST={value} is not a plain http address"))
        } else {
            let authority = without_scheme.split('/').next().unwrap_or_default();
            local_address(authority)
                .ok_or_else(|| format!("OLLAMA_HOST={value} is not this machine"))
        };
        Self { address }
    }

    /// The tags the running Ollama lists.
    fn tags(&self) -> Result<Vec<Tag>, ApiError> {
        let address = self.address.clone().map_err(ApiError::NotLoopback)?;
        let (status, body) = request(address, "GET", "/api/tags", None, TAGS_TIMEOUT)
            .map_err(ApiError::NotRunning)?;
        if status != 200 {
            return Err(ApiError::NotRunning(format!(
                "Ollama answered {status} when asked for its models"
            )));
        }
        serde_json::from_slice::<Listing>(&body)
            .map(|listing| listing.models)
            .map_err(|error| ApiError::NotRunning(format!("unreadable model list ({error})")))
    }

    /// The manifest SHA-256 Ollama lists for `tag`, or `None` if unlisted.
    ///
    /// # Errors
    /// The API is unreachable or not on this machine.
    pub(super) fn listed_manifest(&self, tag: &str) -> Result<Option<String>, String> {
        let listed = self.tags().map_err(|error| match error {
            ApiError::NotLoopback(reason) | ApiError::NotRunning(reason) => reason,
        })?;
        Ok(listed
            .into_iter()
            .find(|listed| listed.name == tag)
            .map(|listed| listed.digest))
    }

    /// Asks Ollama to delete one tag.
    ///
    /// # Errors
    /// The API is unreachable or answered with anything but 200.
    pub fn delete(&self, tag: &str) -> Result<(), String> {
        let address = self.address.clone()?;
        let body = serde_json::json!({ "model": tag, "name": tag }).to_string();
        let (status, response) = request(
            address,
            "DELETE",
            "/api/delete",
            Some(body.as_bytes()),
            DELETE_TIMEOUT,
        )?;
        if status == 200 {
            return Ok(());
        }
        Err(format!(
            "Ollama refused to delete {tag} ({status}: {})",
            String::from_utf8_lossy(&response).trim()
        ))
    }
}

/// The socket address for `host[:port]` if it is on this machine.
fn local_address(authority: &str) -> Option<SocketAddr> {
    let (host, port) = split_port(authority)?;
    let ip = match host {
        "" | "localhost" => IpAddr::V4(Ipv4Addr::LOCALHOST),
        literal => literal.parse::<IpAddr>().ok()?,
    };
    let ip = match ip {
        IpAddr::V4(v4) if v4.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(v6) if v6.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        other => other,
    };
    ip.is_loopback().then_some(SocketAddr::new(ip, port))
}

/// Splits `host:port`, `[v6]:port`, or a bare host (default port).
fn split_port(authority: &str) -> Option<(&str, u16)> {
    if let Some(rest) = authority.strip_prefix('[') {
        let (host, after) = rest.split_once(']')?;
        let port = match after.strip_prefix(':') {
            Some(port) => port.parse().ok()?,
            None if after.is_empty() => DEFAULT_PORT,
            None => return None,
        };
        return Some((host, port));
    }
    match authority.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') => Some((host, port.parse().ok()?)),
        _ => Some((authority, DEFAULT_PORT)),
    }
}

/// One HTTP/1.1 exchange; returns the status and the decoded body.
fn request(
    address: SocketAddr,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
    timeout: Duration,
) -> Result<(u16, Vec<u8>), String> {
    let unreachable = |error: std::io::Error| format!("Ollama is not running ({error})");
    let mut stream = TcpStream::connect_timeout(&address, TAGS_TIMEOUT).map_err(unreachable)?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(unreachable)?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(unreachable)?;
    let body = body.unwrap_or_default();
    let mut message = format!(
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    message.extend_from_slice(body);
    stream.write_all(&message).map_err(unreachable)?;
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .map_err(|error| format!("Ollama did not answer in time ({error})"))?;
    parse_response(&response)
}

/// Splits a raw response into its status and its body, de-chunking it.
fn parse_response(response: &[u8]) -> Result<(u16, Vec<u8>), String> {
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or("Ollama sent an incomplete response")?;
    let head = String::from_utf8_lossy(&response[..split]);
    let body = &response[split + 4..];
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or("Ollama sent no status")?;
    let chunked = head.lines().any(|line| {
        let line = line.to_ascii_lowercase();
        line.starts_with("transfer-encoding:") && line.contains("chunked")
    });
    let body = if chunked {
        dechunk(body)?
    } else {
        body.to_vec()
    };
    Ok((status, body))
}

/// Decodes a `Transfer-Encoding: chunked` body.
fn dechunk(mut body: &[u8]) -> Result<Vec<u8>, String> {
    let mut decoded = Vec::new();
    loop {
        let line_end = body
            .windows(2)
            .position(|window| window == b"\r\n")
            .ok_or("truncated chunk")?;
        let size_text = String::from_utf8_lossy(&body[..line_end]);
        let size_hex = size_text.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size_hex, 16).map_err(|_bad| "bad chunk size")?;
        body = &body[line_end + 2..];
        if size == 0 {
            return Ok(decoded);
        }
        let chunk = body.get(..size).ok_or("truncated chunk")?;
        decoded.extend_from_slice(chunk);
        body = body.get(size + 2..).ok_or("truncated chunk")?;
    }
}

/// Plans removing Ollama blobs: through tags, or directly when orphaned.
pub(super) fn plan(targets: Option<&Vec<Target>>, api: &Api, draft: &mut Draft) {
    let Some(targets) = targets else { return };
    let mut listing = None;
    let mut by_root: BTreeMap<PathBuf, Vec<&Target>> = BTreeMap::new();
    for target in targets {
        if in_blobs_folder(target) {
            by_root
                .entry(target.root.root.clone())
                .or_default()
                .push(target);
        } else {
            draft.refuse(
                RefusalCode::OutsideRoots,
                &target.reference.path,
                "not in the blobs folder of the Ollama models folder",
            );
        }
    }
    for (root, targets) in by_root {
        let scan = manifest_scan(&root);
        let tags = &scan.manifests;
        let removing: HashSet<&Digest> = targets.iter().map(|target| &target.digest).collect();
        let mut deleting: Vec<&ManifestInfo> = Vec::new();
        for target in targets.iter().copied() {
            let users: Vec<&ManifestInfo> = tags
                .iter()
                .filter(|manifest| manifest.digests.contains(&target.digest))
                .collect();
            if users.is_empty() {
                plan_orphan(target, &root, &scan.unreadable, draft);
                continue;
            }
            let listed = listing.get_or_insert_with(|| api.tags());
            if !confirm_listed(target, &users, listed, draft) {
                continue;
            }
            for manifest in users {
                if !deleting.contains(&manifest) {
                    deleting.push(manifest);
                }
            }
            draft
                .removals
                .insert(target.reference.path.clone(), Removal::Delete);
        }
        for manifest in &deleting {
            let covered: Vec<&Target> = targets
                .iter()
                .copied()
                .filter(|target| manifest.digests.contains(&target.digest))
                .collect();
            let Some(first) = covered.first() else {
                continue;
            };
            let on_disk = Digest::sha256_file(&manifest.path)
                .map(|digest| digest.to_hex())
                .unwrap_or_default();
            let mut guards = vec![Guard::OllamaTag {
                tag: manifest.api_name.clone(),
                manifest: on_disk,
            }];
            guards.extend(covered.iter().map(|target| target.guard()));
            draft.step(
                first,
                Action::OllamaDelete {
                    tag: manifest.api_name.clone(),
                },
                &manifest.path,
                guards,
            );
        }
        add_collateral(&root, tags, &deleting, &removing, draft);
    }
}

/// Checks the running Ollama lists every tag as read from disk.
fn confirm_listed(
    target: &Target,
    users: &[&ManifestInfo],
    listed: &Result<Vec<Tag>, ApiError>,
    draft: &mut Draft,
) -> bool {
    let path = &target.reference.path;
    let listed = match listed {
        Ok(listed) => listed,
        Err(ApiError::NotLoopback(reason)) => {
            draft.refuse(RefusalCode::OllamaNotLoopback, path, reason.clone());
            return false;
        }
        Err(ApiError::NotRunning(reason)) => {
            draft.refuse(
                RefusalCode::OllamaNotRunning,
                path,
                format!("open Ollama first: removing its models goes through it ({reason})"),
            );
            return false;
        }
    };
    for manifest in users {
        let on_disk = Digest::sha256_file(&manifest.path)
            .map(|digest| digest.to_hex())
            .unwrap_or_default();
        draft.fingerprint(format!("manifest|{}|{on_disk}", manifest.path.display()));
        let matches = listed
            .iter()
            .any(|tag| tag.name == manifest.api_name && tag.digest == on_disk);
        if !matches {
            draft.refuse(
                RefusalCode::OllamaStoreMismatch,
                path,
                format!(
                    "the running Ollama does not list {} as stored here; it may use another \
                     models folder (OLLAMA_MODELS)",
                    manifest.api_name
                ),
            );
            return false;
        }
    }
    true
}

/// Whether the target is `<models root>/blobs/<file>`.
fn in_blobs_folder(target: &Target) -> bool {
    let blobs = target.reference.path.parent();
    blobs.is_some_and(|blobs| {
        blobs.file_name().is_some_and(|name| name == "blobs")
            && blobs.parent() == Some(target.root.root.as_path())
    })
}

/// A blob no manifest names: deleted directly once surely unused.
///
/// "No manifest names it" is only proven when every manifest could be
/// read; an unreadable one (or one caught mid-write by a pull) might.
fn plan_orphan(target: &Target, root: &Path, unreadable: &[PathBuf], draft: &mut Draft) {
    let blob = &target.reference.path;
    if let Some(manifest) = unreadable.first() {
        draft.refuse(
            RefusalCode::Unreadable,
            blob,
            format!(
                "no tag names it, but the Ollama manifest {} could not be read, so it may",
                manifest.display()
            ),
        );
        return;
    }
    let partial = format!("sha256-{}-partial", target.digest.to_hex());
    let downloading = blob
        .parent()
        .and_then(|blobs| std::fs::read_dir(blobs).ok())
        .is_some_and(|entries| {
            entries
                .filter_map(Result::ok)
                .any(|entry| entry.file_name().to_string_lossy().starts_with(&partial))
        });
    let fresh = std::fs::metadata(blob)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_none_or(|age| age < modeld_core::consolidate::RECENT_WRITE_WINDOW);
    if downloading || fresh {
        draft.refuse(
            RefusalCode::DownloadInProgress,
            blob,
            "Ollama may still be downloading it",
        );
        return;
    }
    let guards = vec![
        target.guard(),
        Guard::OllamaOrphan {
            root: root.to_path_buf(),
            digest: target.digest.clone(),
        },
    ];
    draft.step(target, Action::Delete, blob, guards);
    draft.removals.insert(blob.clone(), Removal::Delete);
}

/// Blobs only the deleted tags use, which Ollama deletes along with them.
fn add_collateral(
    root: &Path,
    tags: &[ManifestInfo],
    deleting: &[&ManifestInfo],
    removing: &HashSet<&Digest>,
    draft: &mut Draft,
) {
    let kept: HashSet<&Digest> = tags
        .iter()
        .filter(|manifest| !deleting.contains(manifest))
        .flat_map(|manifest| &manifest.digests)
        .collect();
    let mut seen = HashSet::new();
    for manifest in deleting {
        for digest in &manifest.digests {
            if removing.contains(digest) || kept.contains(digest) || !seen.insert(digest) {
                continue;
            }
            let blob = root
                .join("blobs")
                .join(format!("sha256-{}", digest.to_hex()));
            let Ok(metadata) = std::fs::metadata(&blob) else {
                continue;
            };
            draft.collateral.push((
                Collateral {
                    path: blob,
                    size: metadata.len(),
                    reason: format!("only {} uses it", manifest.api_name),
                },
                Removal::Delete,
            ));
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    #[test]
    fn ollama_host_must_be_this_machine() {
        let parsed = |value: &str| Api::parse(Some(value)).address;

        assert_eq!(
            Api::parse(None).address,
            Ok(SocketAddr::from(([127, 0, 0, 1], DEFAULT_PORT)))
        );
        assert_eq!(
            parsed("0.0.0.0:8080"),
            Ok(SocketAddr::from(([127, 0, 0, 1], 8080)))
        );
        assert_eq!(
            parsed("http://localhost:9000"),
            Ok(SocketAddr::from(([127, 0, 0, 1], 9000)))
        );
        assert_eq!(
            parsed("[::1]:7000"),
            Ok(SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 7000))
        );
        assert!(parsed("192.168.1.20:11434").is_err());
        assert!(parsed("ollama.example.com").is_err());
        assert!(parsed("https://127.0.0.1:11434").is_err());
    }

    #[test]
    fn chunked_bodies_are_reassembled() {
        let response = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{\"a\"\r\n3\r\n:1}\r\n0\r\n\r\n";

        assert_eq!(parse_response(response), Ok((200, b"{\"a\":1}".to_vec())));
    }

    /// An Ollama stand-in: lists tags, and deletes like Ollama does.
    pub(in crate::remove) struct FakeOllama {
        pub api: Api,
        pub deleted: Arc<Mutex<Vec<String>>>,
    }

    impl FakeOllama {
        /// Serves the tags (name, manifest file) of the models `root`.
        pub(in crate::remove) fn serve(root: PathBuf, tags: Vec<(String, PathBuf)>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            let address = listener.local_addr().expect("address");
            let deleted = Arc::new(Mutex::new(Vec::new()));
            let log = Arc::clone(&deleted);
            std::thread::spawn(move || {
                for stream in listener.incoming().filter_map(Result::ok) {
                    answer(stream, &root, &tags, &log);
                }
            });
            Self {
                api: Api {
                    address: Ok(address),
                },
                deleted,
            }
        }
    }

    fn answer(
        mut stream: TcpStream,
        root: &Path,
        tags: &[(String, PathBuf)],
        deleted: &Mutex<Vec<String>>,
    ) {
        let request = read_request(&mut stream);
        let body = if request.starts_with("GET /api/tags") {
            let models: Vec<serde_json::Value> = tags
                .iter()
                .filter(|(_, manifest)| manifest.exists())
                .map(|(name, manifest)| {
                    let digest = Digest::sha256_file(manifest)
                        .expect("hash manifest")
                        .to_hex();
                    serde_json::json!({ "name": name, "model": name, "digest": digest })
                })
                .collect();
            serde_json::json!({ "models": models }).to_string()
        } else {
            let name = tags
                .iter()
                .find(|(name, _)| request.contains(&format!("\"model\":\"{name}\"")));
            if let Some((name, manifest)) = name {
                let _ = std::fs::remove_file(manifest);
                prune_unused_blobs(root);
                deleted.lock().expect("lock").push(name.clone());
            }
            String::new()
        };
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
    }

    /// Reads one request: its headers, then `Content-Length` body bytes.
    fn read_request(stream: &mut TcpStream) -> String {
        let mut request = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            let read = stream.read(&mut buffer).unwrap_or(0);
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);
            let text = String::from_utf8_lossy(&request);
            let Some(head_end) = text.find("\r\n\r\n") else {
                continue;
            };
            let length = text[..head_end]
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|n| n.trim().to_string())
                })
                .and_then(|n| n.parse::<usize>().ok())
                .unwrap_or(0);
            if request.len() >= head_end + 4 + length {
                break;
            }
        }
        String::from_utf8_lossy(&request).into_owned()
    }

    /// Deletes blobs no manifest uses anymore, as `ollama rm` does.
    fn prune_unused_blobs(root: &Path) {
        let used: HashSet<Digest> = modeld_providers::ollama::manifests(root)
            .into_iter()
            .flat_map(|manifest| manifest.digests)
            .collect();
        let Ok(entries) = std::fs::read_dir(root.join("blobs")) else {
            return;
        };
        for entry in entries.filter_map(Result::ok) {
            let name = entry.file_name();
            let digest = name
                .to_str()
                .and_then(modeld_providers::ollama::digest_from_blob_name);
            if digest.is_some_and(|digest| !used.contains(&digest)) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    /// An API address nothing listens on.
    pub(in crate::remove) fn stopped_ollama() -> Api {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("address");
        drop(listener);
        Api {
            address: Ok(address),
        }
    }

    /// An API on another machine.
    pub(in crate::remove) fn remote_ollama() -> Api {
        Api::parse(Some("10.0.0.2:11434"))
    }
}
