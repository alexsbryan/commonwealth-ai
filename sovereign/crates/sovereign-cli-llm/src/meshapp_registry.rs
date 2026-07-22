// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn meshapp {publish,install,list}` — the curated registry client.
//!
//! A mesh app is distributed as a self-contained `tar.zst`: the bundle (its
//! `index.html` / `app.js` / `meshapp.json` / `recipe.toml`) plus a copy of the
//! shared `_sdk/`, so an installed app runs without a repo checkout. Apps
//! install under `~/.sovereign/meshapps/` (the bundle at `<id>/`, the SDK as a
//! shared `_sdk/` sibling — matching the bundles' `../_sdk/` imports), and
//! `meshapp dev <id>` runs them from there.
//!
//! TRUST is **integrity** (sha256 of the artifact) + **curation** (membership
//! in the reviewed in-repo `meshapp-registry.toml`). Cryptographic signing
//! (ed25519, already a dep) is the documented next step. Until the no-IPC
//! bridge ships, curation IS the security model: an installed bundle runs in a
//! webview with IPC, so only reviewed/known apps should be installed — the CLI
//! warns when sideloading an app that isn't in the curated registry.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use sovereign_cli_shared::dirs::sovereign_meshapps;

/// The reviewed, in-repo curated list (the trust anchor). Resolved relative to
/// the repo root (run from there) or via `--registry`.
const CURATED_REGISTRY: &str = "sovereign-recipes/meshapp-registry.toml";
const DEFAULT_BUNDLE_BASE: &str = "sovereign/crates/sovereign-desktop/public/meshapp";

#[derive(Debug, Default, Serialize, Deserialize)]
struct Registry {
    #[serde(default)]
    apps: Vec<RegistryEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RegistryEntry {
    id: String,
    name: String,
    version: String,
    corpus: String,
    /// sha256 of the bundle `tar.zst` — integrity.
    sha256: String,
    /// `curated` (in the reviewed in-repo registry) | `unsigned` (sideloaded).
    #[serde(default = "unsigned")]
    trust: String,
    /// Where to fetch the artifact. `path` for a locally-published tar; `url`
    /// for a download. Exactly one is set per source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    url: Option<String>,
}

fn unsigned() -> String {
    "unsigned".to_string()
}

#[derive(Deserialize)]
struct Manifest {
    id: String,
    name: String,
    version: String,
    corpus: String,
}

fn read_manifest(bundle_dir: &Path) -> Result<Manifest, String> {
    let p = bundle_dir.join("meshapp.json");
    let bytes = std::fs::read(&p).map_err(|e| format!("read {}: {e}", p.display()))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("parse {}: {e}", p.display()))
}

/// `(bundle_dir, sdk_dir)` for an app id — default in-repo, or `--dir`.
fn resolve_bundle(app_id: &str, dir: Option<PathBuf>) -> Result<(PathBuf, PathBuf), String> {
    let bundle_dir = dir.unwrap_or_else(|| PathBuf::from(DEFAULT_BUNDLE_BASE).join(app_id));
    if !bundle_dir.join("meshapp.json").is_file() {
        return Err(format!(
            "no meshapp.json in {} — pass --dir <bundle-dir> (or run from the repo root)",
            bundle_dir.display()
        ));
    }
    let sdk_dir = bundle_dir
        .parent()
        .map(|p| p.join("_sdk"))
        .unwrap_or_else(|| PathBuf::from("_sdk"));
    if !sdk_dir.join("meshapp.js").is_file() {
        return Err(format!(
            "no _sdk next to the bundle (looked in {})",
            sdk_dir.display()
        ));
    }
    Ok((bundle_dir, sdk_dir))
}

// ─── publish ─────────────────────────────────────────────────────────

pub fn publish(args: &[String]) -> i32 {
    let mut app_id: Option<String> = None;
    let mut dir: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--dir" => {
                dir = args.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            "--out" => {
                out = args.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            o if app_id.is_none() && !o.starts_with("--") => {
                app_id = Some(o.to_string());
                i += 1;
            }
            o => {
                eprintln!("meshapp publish: unexpected argument `{o}`");
                return 2;
            }
        }
    }
    let Some(app_id) = app_id else {
        eprintln!("meshapp publish: missing <app-id>");
        return 2;
    };
    let (bundle_dir, sdk_dir) = match resolve_bundle(&app_id, dir) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("meshapp publish: {e}");
            return 1;
        }
    };
    let manifest = match read_manifest(&bundle_dir) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("meshapp publish: {e}");
            return 1;
        }
    };

    let out_dir = out.unwrap_or_else(|| sovereign_meshapps().join("artifacts"));
    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        eprintln!("meshapp publish: create {}: {e}", out_dir.display());
        return 1;
    }
    let artifact = out_dir.join(format!("{}-{}.tar.zst", manifest.id, manifest.version));
    if let Err(e) = pack_bundle(&bundle_dir, &sdk_dir, &manifest.id, &artifact) {
        eprintln!("meshapp publish: pack: {e}");
        return 1;
    }
    let sha = match sha256_file(&artifact) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("meshapp publish: sha256: {e}");
            return 1;
        }
    };

    // Record it in the local registry so `meshapp install <id>` resolves it.
    let entry = RegistryEntry {
        id: manifest.id.clone(),
        name: manifest.name.clone(),
        version: manifest.version.clone(),
        corpus: manifest.corpus.clone(),
        sha256: sha.clone(),
        trust: "unsigned".into(),
        path: Some(artifact.display().to_string()),
        url: None,
    };
    if let Err(e) = upsert_local(&entry) {
        eprintln!("meshapp publish: {e}");
        return 1;
    }

    let size = std::fs::metadata(&artifact).map(|m| m.len()).unwrap_or(0);
    println!(
        "published `{}` v{} → {}",
        manifest.id,
        manifest.version,
        artifact.display()
    );
    println!("  sha256 {sha}");
    println!("  size   {:.1} KB", size as f64 / 1024.0);
    println!(
        "\ninstall locally:  svrn meshapp install {}",
        manifest.id
    );
    println!("to publish to the CURATED registry (so others can install + one-click data):");
    println!("  1. upload the tar to a URL or HuggingFace");
    println!("  2. add an [[apps]] entry to {CURATED_REGISTRY} (PR for review) with:");
    println!("       id={:?} version={:?} corpus={:?} sha256={:?} url=\"<download-url>\" trust=\"curated\"",
        manifest.id, manifest.version, manifest.corpus, sha);
    0
}

fn pack_bundle(bundle_dir: &Path, sdk_dir: &Path, app_id: &str, out: &Path) -> io::Result<()> {
    let file = File::create(out)?;
    let enc = zstd::stream::Encoder::new(file, 9)?;
    let mut tar = tar::Builder::new(enc);
    tar.append_dir_all(app_id, bundle_dir)?; // "<id>/index.html", …
    tar.append_dir_all("_sdk", sdk_dir)?; // "_sdk/meshapp.js", …
    tar.into_inner()?.finish()?; // finish tar, then finalize zstd
    Ok(())
}

fn sha256_file(path: &Path) -> io::Result<String> {
    let mut f = File::open(path)?;
    let mut hasher = Sha256::new();
    io::copy(&mut f, &mut hasher)?;
    Ok(hex::encode(hasher.finalize()))
}

// ─── install ─────────────────────────────────────────────────────────

pub async fn install(args: &[String]) -> i32 {
    let mut app_id: Option<String> = None;
    let mut from: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--from" => {
                from = args.get(i + 1).cloned();
                i += 2;
            }
            o if app_id.is_none() && !o.starts_with("--") => {
                app_id = Some(o.to_string());
                i += 1;
            }
            o => {
                eprintln!("meshapp install: unexpected argument `{o}`");
                return 2;
            }
        }
    }
    let Some(app_id) = app_id else {
        eprintln!("meshapp install: missing <app-id>");
        return 2;
    };

    // Resolve the source + the expected sha + trust.
    let (source, expected_sha, trust): (Source, Option<String>, &str) = if let Some(from) = from {
        eprintln!("meshapp install: sideloading `{app_id}` from {from} — not curated; only install apps you trust.");
        (source_of(&from), None, "unsigned")
    } else {
        match resolve_entry(&app_id) {
            Some((entry, curated)) => {
                let src = if let Some(p) = &entry.path {
                    Source::Path(PathBuf::from(p))
                } else if let Some(u) = &entry.url {
                    Source::Url(u.clone())
                } else {
                    eprintln!("meshapp install: registry entry for `{app_id}` has no path or url");
                    return 1;
                };
                (
                    src,
                    Some(entry.sha256.clone()),
                    if curated { "curated" } else { "unsigned" },
                )
            }
            None => {
                eprintln!("meshapp install: `{app_id}` is not in the registry — pass --from <path|url> to sideload.");
                return 1;
            }
        }
    };

    // Fetch the artifact to a temp file.
    let tmp = match fetch(&source).await {
        Ok(t) => t,
        Err(e) => {
            eprintln!("meshapp install: fetch: {e}");
            return 1;
        }
    };
    // Verify integrity.
    if let Some(want) = &expected_sha {
        match sha256_file(tmp.path()) {
            Ok(got) if &got == want => {}
            Ok(got) => {
                eprintln!("meshapp install: sha256 mismatch (want {want}, got {got}) — refusing");
                return 1;
            }
            Err(e) => {
                eprintln!("meshapp install: sha256: {e}");
                return 1;
            }
        }
    }
    // Unpack into ~/.sovereign/meshapps/ (creates <id>/ and _sdk/).
    let dest = sovereign_meshapps();
    if let Err(e) = unpack_bundle(tmp.path(), &dest) {
        eprintln!("meshapp install: unpack: {e}");
        return 1;
    }
    println!(
        "installed `{app_id}` ({trust}) → {}",
        dest.join(&app_id).display()
    );
    println!("  run it:  svrn meshapp dev {app_id}");
    println!("  or open it from the Mesh apps section in Sovereign Desktop.");
    0
}

enum Source {
    Path(PathBuf),
    Url(String),
}

fn source_of(s: &str) -> Source {
    if s.starts_with("http://") || s.starts_with("https://") {
        Source::Url(s.to_string())
    } else {
        Source::Path(PathBuf::from(s))
    }
}

/// Download (URL) or copy (path) the artifact into a temp file.
async fn fetch(source: &Source) -> Result<tempfile::NamedTempFile, String> {
    let tmp = tempfile::NamedTempFile::new().map_err(|e| format!("temp file: {e}"))?;
    match source {
        Source::Path(p) => {
            std::fs::copy(p, tmp.path()).map_err(|e| format!("read {}: {e}", p.display()))?;
        }
        Source::Url(u) => {
            let bytes = reqwest::get(u)
                .await
                .map_err(|e| format!("GET {u}: {e}"))?
                .error_for_status()
                .map_err(|e| format!("GET {u}: {e}"))?
                .bytes()
                .await
                .map_err(|e| format!("read body: {e}"))?;
            std::fs::write(tmp.path(), &bytes).map_err(|e| format!("write temp: {e}"))?;
        }
    }
    Ok(tmp)
}

fn unpack_bundle(tar_path: &Path, dest: &Path) -> io::Result<()> {
    let file = File::open(tar_path)?;
    let dec = zstd::stream::Decoder::new(file)?;
    let mut archive = tar::Archive::new(dec);
    std::fs::create_dir_all(dest)?;
    // `tar`'s unpack rejects absolute paths and `..` traversal by default.
    archive.unpack(dest)
}

// ─── list ────────────────────────────────────────────────────────────

pub fn list(_args: &[String]) -> i32 {
    let installed = installed_ids();
    println!("Available (curated registry):");
    let curated = load_registry_file(&PathBuf::from(CURATED_REGISTRY));
    if curated.apps.is_empty() {
        println!("  (none — {CURATED_REGISTRY} is empty or not found from here)");
    }
    for a in &curated.apps {
        let mark = if installed.contains(&a.id) {
            "✓ installed"
        } else {
            ""
        };
        println!("  {:<18} v{:<8} [{}]  {}", a.id, a.version, a.trust, mark);
    }
    let local = load_registry_file(&local_registry_path());
    if !local.apps.is_empty() {
        println!("\nPublished locally:");
        for a in &local.apps {
            let mark = if installed.contains(&a.id) {
                "✓ installed"
            } else {
                ""
            };
            println!("  {:<18} v{:<8} [{}]  {}", a.id, a.version, a.trust, mark);
        }
    }
    println!("\nInstalled ({}):", sovereign_meshapps().display());
    if installed.is_empty() {
        println!("  (none yet — `svrn meshapp install <id>`)");
    }
    for id in &installed {
        println!("  {id}");
    }
    0
}

fn installed_ids() -> Vec<String> {
    let dir = sovereign_meshapps();
    let mut ids = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            if !e.path().is_dir() {
                continue;
            }
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('_') || name == "artifacts" {
                continue;
            }
            if e.path().join("meshapp.json").is_file() {
                ids.push(name);
            }
        }
    }
    ids.sort();
    ids
}

// ─── registry files ──────────────────────────────────────────────────

fn local_registry_path() -> PathBuf {
    sovereign_meshapps().join("registry.toml")
}

fn load_registry_file(path: &Path) -> Registry {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default()
}

/// Resolve an app id to its entry, preferring the LOCAL registry (your own
/// published apps) then the CURATED in-repo registry. Returns `(entry, curated)`.
fn resolve_entry(app_id: &str) -> Option<(RegistryEntry, bool)> {
    let local = load_registry_file(&local_registry_path());
    if let Some(e) = local.apps.into_iter().find(|a| a.id == app_id) {
        return Some((e, false));
    }
    let curated = load_registry_file(&PathBuf::from(CURATED_REGISTRY));
    curated
        .apps
        .into_iter()
        .find(|a| a.id == app_id)
        .map(|e| (e, true))
}

fn upsert_local(entry: &RegistryEntry) -> Result<(), String> {
    let path = local_registry_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let mut reg = load_registry_file(&path);
    reg.apps.retain(|a| a.id != entry.id);
    reg.apps.push(entry.clone());
    reg.apps.sort_by(|a, b| a.id.cmp(&b.id));
    let toml = toml::to_string_pretty(&reg).map_err(|e| format!("serialize registry: {e}"))?;
    std::fs::write(&path, toml).map_err(|e| format!("write {}: {e}", path.display()))
}
