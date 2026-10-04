// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn setup --hosted <vendor>`: a node whose chat model is hosted.
//!
//! Chat goes to the vendor. Embeddings run on this machine, from the small
//! embedding GGUF this command downloads, so text is never sent out to be
//! embedded. Writing a vendor into `[engine]` is the operator's consent for
//! this node's turns to go there (`oicp_client::FarEnd`). The node holds no
//! chat weights, so no `[models]` is written.
//!
//! The key is read from stdin, never argv: `svrn setup --hosted deepseek <
//! key.txt`, or paste it at the prompt.

use std::io::{BufRead as _, IsTerminal as _};
use std::path::Path;

use sovereign_contracts::daemon_wire::ProbedPlan;
use sovereign_core::setup_config::{DataSection, EngineKind, EngineSection, SetupConfig};

use super::{
    download::download_with_progress, probe, run_config_path, run_data_dir, url_for, Opts,
};

/// A vendor this command knows by name, or any OpenAI-compatible base URL.
struct Vendor {
    name: String,
    endpoint: String,
    model: Option<&'static str>,
    context_size: u32,
    extra_params: Option<serde_json::Value>,
}

fn vendor(raw: &str) -> Result<Vendor, String> {
    let named = |endpoint: &str, model, extra_params| Vendor {
        name: raw.to_string(),
        endpoint: endpoint.to_string(),
        model,
        context_size: 65536,
        extra_params,
    };
    match raw {
        "deepseek" => Ok(named(
            "https://api.deepseek.com/v1",
            Some("deepseek-flash"),
            None,
        )),
        // Without `require_parameters` OpenRouter may route a schema request
        // to a backend that ignores the schema, and that loss is silent.
        "openrouter" => Ok(named(
            "https://openrouter.ai/api/v1",
            None,
            Some(serde_json::json!({ "provider": { "require_parameters": true } })),
        )),
        url if url.starts_with("https://") || url.starts_with("http://") => Ok(Vendor {
            name: url.to_string(),
            endpoint: url.trim_end_matches('/').to_string(),
            model: None,
            context_size: 32768,
            extra_params: None,
        }),
        other => Err(format!(
            "--hosted {other}: name a vendor (deepseek, openrouter) or an OpenAI-compatible \
             base URL ending in /v1"
        )),
    }
}

/// The key, from a pipe or a paste. Refused when empty rather than written
/// as a config that fails on its first turn.
fn read_key(vendor: &str) -> Result<String, String> {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        eprintln!("  Paste your {vendor} API key and press Enter (it is shown as you paste):");
    }
    let mut key = String::new();
    stdin
        .lock()
        .read_line(&mut key)
        .map_err(|e| format!("reading the API key from stdin: {e}"))?;
    let key = key.trim().to_string();
    if key.is_empty() {
        return Err("no API key on stdin: pipe it in (`< key.txt`) or paste it".to_string());
    }
    Ok(key)
}

/// `config.toml` carries the key, so only its owner reads it.
fn owner_only(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("chmod 600 {}: {e}", path.display()))?;
    }
    Ok(())
}

pub(super) async fn run_hosted_setup(raw: &str, opts: &Opts) -> i32 {
    match hosted_setup(raw, opts).await {
        Ok(()) => 0,
        Err(msg) => {
            eprintln!("error: {msg}");
            1
        }
    }
}

async fn hosted_setup(raw: &str, opts: &Opts) -> Result<(), String> {
    let vendor = vendor(raw)?;
    let model = opts
        .model
        .clone()
        .or(vendor.model.map(str::to_string))
        .ok_or_else(|| {
            format!(
                "--hosted {}: pass --model <id>, the name it serves",
                vendor.name
            )
        })?;
    let cfg_path = run_config_path(opts);
    if cfg_path.exists() && !opts.reset {
        println!("  Already set up. Config at {}", cfg_path.display());
        println!("  Run `svrn setup --reset --hosted {raw}` to reconfigure.");
        return Ok(());
    }

    println!();
    println!("  Sovereign Setup — hosted model");
    println!("  {}", "\u{2500}".repeat(54));
    println!();
    println!("  Your questions, and text from the folders you add, go to");
    println!("  {} ({model}) to be answered.", vendor.endpoint);
    println!("  Embeddings stay on this machine: a small model, downloaded now.");
    println!();
    let key = read_key(&vendor.name)?;

    // The embed model the wizard prescribes, so this node's corpora share a
    // vector space with every other node and every prebuilt corpus.
    let probed = tokio::task::spawn_blocking(|| probe::ask::<ProbedPlan>("plan", &[]))
        .await
        .map_err(|e| format!("the setup probe did not finish: {e}"))??;
    let embed = probed
        .plan
        .embed
        .ok_or("the bundled manifest names no embed model for this machine")?;
    let url = url_for(&probed.urls, &embed)?;
    let data_dir = run_data_dir(opts);
    let models_dir = data_dir.join("models");
    std::fs::create_dir_all(&models_dir)
        .map_err(|e| format!("cannot create {}: {e}", models_dir.display()))?;
    let embed_path = models_dir.join(&embed.file);
    download_with_progress(&url, &embed_path, &embed.file, embed.size_gb, None).await?;

    let client_port = opts
        .client_port
        .unwrap_or_else(sovereign_contracts::setup_config::default_client_port);
    let cfg = hosted_config(
        vendor,
        model,
        key,
        opts.fast_model.clone(),
        embed_path,
        data_dir,
        client_port,
    );
    cfg.save_to(&cfg_path)?;
    owner_only(&cfg_path)?;
    println!(
        "    \u{2713} Wrote {} (readable by you only)",
        cfg_path.display()
    );
    println!();
    println!("  Next:");
    println!("    svrn daemon");
    println!(
        "  Then ask it anything, or ingest a folder. The key is first used on the first \
         answer, so a wrong key or model shows there."
    );
    Ok(())
}

/// The config this command writes: no `[models]`, the vendor as a remote
/// `[engine]` that embeds from `embed_path` in this process.
fn hosted_config(
    vendor: Vendor,
    model: String,
    key: String,
    fast_model: Option<String>,
    embed_path: std::path::PathBuf,
    data_dir: std::path::PathBuf,
    client_port: u16,
) -> SetupConfig {
    let mut cfg = SetupConfig::unconfigured();
    cfg.models = None;
    cfg.engine = EngineSection {
        kind: EngineKind::Remote,
        endpoint: Some(vendor.endpoint),
        api_key: Some(key),
        model_id: Some(model),
        fast_model_id: fast_model,
        context_size: vendor.context_size,
        embed_path: Some(embed_path),
        extra_params: vendor.extra_params,
        ..Default::default()
    };
    cfg.daemon.client_port = client_port;
    cfg.daemon.internal_port = client_port + 1;
    cfg.data = DataSection { dir: data_dir };
    cfg
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What this command writes, the daemon loads: a node whose chat model
    /// is hosted serves its own turns and names its embed space from the
    /// local GGUF. The boot refused this file until the class check learned
    /// that a remote engine needs no `[models]`.
    #[test]
    fn the_written_config_loads_as_a_holder() {
        use sovereign_core::setup_config::NodeClass;
        let dir = tempfile::tempdir().expect("tempdir");
        let embed = dir.path().join("models/Qwen3-Embedding-0.6B-Q8_0.gguf");
        let cfg = hosted_config(
            vendor("deepseek").unwrap(),
            "deepseek-flash".into(),
            "not-a-key".into(),
            None,
            embed,
            dir.path().to_path_buf(),
            19751,
        );
        let path = SetupConfig::path_in(dir.path());
        cfg.save_to(&path).expect("save");
        let loaded = SetupConfig::load_from(&path).expect("the daemon must load what setup wrote");
        assert_eq!(loaded.node_class(), NodeClass::Holder);
        // The name a caller passes as `model`, and the chat model watched-folder
        // enrichment defaults to: the vendor's, which this node serves as primary.
        assert_eq!(
            loaded.primary_model_stem().as_deref(),
            Some("deepseek-flash")
        );
        assert_eq!(
            loaded.advertised_embed_model_id().as_deref(),
            Some("Qwen3-Embedding-0.6B-Q8_0")
        );
    }

    /// A named vendor carries its base and default model; a URL needs
    /// `--model`; anything else is refused naming what is accepted.
    #[test]
    fn a_vendor_is_a_name_or_a_url() {
        let deepseek = vendor("deepseek").unwrap();
        assert_eq!(deepseek.endpoint, "https://api.deepseek.com/v1");
        assert_eq!(deepseek.model, Some("deepseek-flash"));
        let openrouter = vendor("openrouter").unwrap();
        assert!(
            openrouter.model.is_none(),
            "openrouter has no default model"
        );
        assert!(openrouter.extra_params.is_some());
        let url = vendor("https://api.example.com/v1/").unwrap();
        assert_eq!(url.endpoint, "https://api.example.com/v1");
        let err = vendor("bedrock").err().unwrap();
        assert!(err.contains("deepseek") && err.contains("/v1"), "{err}");
    }
}
