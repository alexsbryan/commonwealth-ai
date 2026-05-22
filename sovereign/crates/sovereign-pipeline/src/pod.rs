//! Pod-lifecycle wrappers for `vastai`.
//!
//! The pipeline crate stays a thin shell over the upstream `vastai`
//! Python CLI — no API client of our own, no version locking. We
//! shell out, parse `--raw` JSON, and persist what we need in the
//! local ledger so `pod down` can find the pod we launched and
//! account for the cost.
//!
//! Why not Vast's REST directly? Two reasons. First, the CLI handles
//! auth (`~/.config/vastai/vast_api_key`) so we never touch the
//! key. Second, the CLI's `search offers <query>` already expresses
//! the rich filter language (`gpu_name=L40S verified=true price<1`);
//! re-implementing that as Rust struct fields would be all churn
//! and no payoff.

use std::process::{Command, Output, Stdio};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PodError {
    #[error("vastai: spawn failed: {0}. Install with `pip install vastai`.")]
    Spawn(std::io::Error),
    #[error("vastai exited {code}: {stderr}")]
    NonZeroExit { code: i32, stderr: String },
    #[error("vastai json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("vastai search returned no offers matching `{0}`")]
    NoOffers(String),
    #[error("ledger: {0}")]
    Ledger(#[from] crate::ledger::LedgerError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, PodError>;

/// Map Vast's `verification: "<status>"` string onto our bool. "verified"
/// is the only status that counts; anything else (missing, "frozen",
/// "deverified", null) yields false.
fn deserialize_verified_from_verification<'de, D>(d: D) -> std::result::Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s: Option<String> = serde::Deserialize::deserialize(d)?;
    Ok(s.as_deref() == Some("verified"))
}

/// One row from `vastai search offers --raw`. Vast returns dozens of
/// fields; we deserialize only what we use. Unknown fields are
/// silently ignored.
#[derive(Debug, Clone, Deserialize)]
pub struct Offer {
    pub id: u64,
    #[serde(default, alias = "dph_total")]
    pub price_per_hour: f64,
    #[serde(default)]
    pub gpu_name: String,
    #[serde(default)]
    pub num_gpus: u32,
    #[serde(default)]
    pub gpu_ram: f64,
    #[serde(default)]
    pub geolocation: String,
    // Vast's offer JSON has no boolean `verified` field — host status
    // is reported as `verification: "<status>"` (string). Only the value
    // "verified" means "host has passed Vast's verification suite";
    // "frozen", "deverified", and friends do not. Deserialize from the
    // string and project onto our bool so pick_offer's ranking and the
    // display layer keep their simple shape. Without this, every offer
    // parsed as `verified=false` regardless of host status, and the
    // verified-first ranking was a no-op — observed in pod-up runs
    // picking unverified offers despite the search asking `verified=true`.
    #[serde(default, rename = "verification", deserialize_with = "deserialize_verified_from_verification")]
    pub verified: bool,
    // Vast emits both `reliability` and `reliability2` in the same offer
    // object (identical values today; reliability2 is the documented "last
    // 90 days" surface). We can't alias both onto this field — serde-json
    // rejects two JSON keys mapping to one struct slot as DuplicateField.
    // Take `reliability` only; `reliability2` is silently ignored.
    #[serde(default)]
    pub reliability: f64,
    #[serde(default)]
    pub cuda_max_good: f64,
}

#[derive(Debug, Clone, Deserialize)]
struct CreateResponse {
    /// Vast's create response shape: `{"success": true, "new_contract": 12345}`.
    /// Older clients emit `"id"`. Accept either via untagged.
    #[serde(default, alias = "new_contract", alias = "id")]
    new_contract: Option<u64>,
    #[serde(default)]
    success: Option<bool>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CreatedInstance {
    pub vast_id: String,
    pub gpu_name: String,
    pub image: String,
    pub cost_per_hour: f64,
}

/// Search the Vast marketplace and return offers ordered ascending by
/// price. Empty `query` falls back to the default verified filter.
pub fn search_offers(query: &str, limit: u32) -> Result<Vec<Offer>> {
    let q = if query.is_empty() {
        "verified=true rentable=true".to_string()
    } else {
        query.to_string()
    };
    let mut cmd = Command::new("vastai");
    cmd.arg("search")
        .arg("offers")
        .arg(&q)
        .arg("--raw")
        .arg("-o")
        .arg("dph_total")
        .arg("--limit")
        .arg(limit.to_string());
    let out = run_vastai(cmd)?;
    let offers: Vec<Offer> = serde_json::from_slice(&out.stdout)?;
    if offers.is_empty() {
        return Err(PodError::NoOffers(q));
    }
    Ok(offers)
}

pub struct CreateRequest<'a> {
    pub offer_id: u64,
    pub image: &'a str,
    pub disk_gb: u32,
    /// Shell string to run on first boot. Embed env-var exports +
    /// `exec /entrypoint.sh` so the container's normal lifecycle
    /// kicks in. (Vast's SSH instance type bypasses image
    /// ENTRYPOINT, so we have to invoke it explicitly.)
    pub onstart_cmd: &'a str,
    /// Optional `--env` blob (Vast uses single-string form, e.g.
    /// `-e MESH_JOIN_LINK=cwth-… -p 8080:8080`).
    pub env: &'a str,
    /// Human label propagated to Vast + ledger.
    pub label: &'a str,
    /// `true` to launch as on-demand SSH (the common case for us);
    /// pass `false` for jupyter / args-style entrypoints.
    pub ssh: bool,
}

/// Create a Vast instance from a chosen offer. Returns the new
/// contract id stringified, plus the gpu name + cost echoed back
/// for the ledger entry.
pub fn create_instance(req: &CreateRequest<'_>, offer: &Offer) -> Result<CreatedInstance> {
    let mut cmd = Command::new("vastai");
    cmd.arg("create")
        .arg("instance")
        .arg(req.offer_id.to_string())
        .arg("--image")
        .arg(req.image)
        .arg("--disk")
        .arg(req.disk_gb.to_string())
        .arg("--onstart-cmd")
        .arg(req.onstart_cmd)
        .arg("--label")
        .arg(req.label)
        .arg("--raw")
        .arg("--cancel-unavail");
    if !req.env.is_empty() {
        cmd.arg("--env").arg(req.env);
    }
    if req.ssh {
        cmd.arg("--ssh");
    }
    let out = run_vastai(cmd)?;
    let resp: CreateResponse = serde_json::from_slice(&out.stdout)?;
    if matches!(resp.success, Some(false)) {
        return Err(PodError::NonZeroExit {
            code: 1,
            stderr: resp.error.unwrap_or_else(|| "create failed".into()),
        });
    }
    let id = resp.new_contract.ok_or_else(|| PodError::NonZeroExit {
        code: 1,
        stderr: format!(
            "create response missing instance id: {}",
            String::from_utf8_lossy(&out.stdout)
        ),
    })?;
    Ok(CreatedInstance {
        vast_id: id.to_string(),
        gpu_name: offer.gpu_name.clone(),
        image: req.image.to_string(),
        cost_per_hour: offer.price_per_hour,
    })
}

/// Destroy a pod. Idempotent — destroying a non-existent id returns
/// success so the ledger can always be cleaned up.
pub fn destroy_instance(vast_id: &str) -> Result<()> {
    let mut cmd = Command::new("vastai");
    cmd.arg("destroy").arg("instance").arg(vast_id).arg("-y");
    // Don't surface Vast's "instance not found" — we want destroy
    // to be reentrant after `pod list --prune`.
    match run_vastai(cmd) {
        Ok(_) => Ok(()),
        Err(PodError::NonZeroExit { stderr, .. }) if stderr.contains("not found") => Ok(()),
        Err(e) => Err(e),
    }
}

fn run_vastai(mut cmd: Command) -> Result<Output> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let out = cmd.output().map_err(PodError::Spawn)?;
    if !out.status.success() {
        return Err(PodError::NonZeroExit {
            code: out.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(out)
}

/// Pick the best offer from a slice. Ranking:
///   1. Verified hosts first.
///   2. Then by `reliability` desc (last 90 days).
///   3. Then by price asc.
///
/// Vast's default ordering (already by price asc when we ask for it)
/// often surfaces unverified or low-reliability hosts; this overlay
/// keeps us off them by default. Override by passing `--raw` flag
/// straight through if you know what you're doing.
pub fn pick_offer<'a>(offers: &'a [Offer]) -> Option<&'a Offer> {
    let mut ranked: Vec<&Offer> = offers.iter().collect();
    ranked.sort_by(|a, b| {
        b.verified
            .cmp(&a.verified)
            .then(b.reliability.partial_cmp(&a.reliability).unwrap_or(std::cmp::Ordering::Equal))
            .then(a.price_per_hour.partial_cmp(&b.price_per_hour).unwrap_or(std::cmp::Ordering::Equal))
    });
    ranked.first().copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offer(id: u64, price: f64, verified: bool, reliability: f64) -> Offer {
        Offer {
            id,
            price_per_hour: price,
            gpu_name: "L40S".into(),
            num_gpus: 1,
            gpu_ram: 48.0,
            geolocation: "US".into(),
            verified,
            reliability,
            cuda_max_good: 12.4,
        }
    }

    #[test]
    fn pick_prefers_verified() {
        let offers = [offer(1, 0.30, false, 0.99), offer(2, 0.50, true, 0.99)];
        let pick = pick_offer(&offers).unwrap();
        assert_eq!(pick.id, 2);
    }

    #[test]
    fn pick_prefers_reliability_then_price_among_verified() {
        let offers = [
            offer(1, 0.30, true, 0.80),
            offer(2, 0.50, true, 0.99),
            offer(3, 0.40, true, 0.99),
        ];
        let pick = pick_offer(&offers).unwrap();
        // Reliability ties → cheapest of the reliable group wins.
        assert_eq!(pick.id, 3);
    }

    #[test]
    fn pick_handles_empty() {
        let pick = pick_offer(&[]);
        assert!(pick.is_none());
    }

    #[test]
    fn offer_reads_verified_from_verification_string() {
        // Real-world Vast response shape: `verification` is a STRING,
        // not a `verified` bool. Captured from `vastai search offers
        // --raw` on 2026-05-15; trimmed to the fields we deserialize.
        let raw = r#"{
            "id": 35153580,
            "dph_total": 0.548,
            "gpu_name": "L40S",
            "num_gpus": 1,
            "gpu_ram": 46068,
            "geolocation": "Texas, US",
            "reliability": 0.9979,
            "verification": "verified",
            "cuda_max_good": 13.0
        }"#;
        let o: Offer = serde_json::from_str(raw).unwrap();
        assert!(o.verified, "verification=\"verified\" must parse as verified=true");
        assert_eq!(o.id, 35153580);
        assert_eq!(o.geolocation, "Texas, US");
    }

    #[test]
    fn offer_treats_non_verified_strings_as_false() {
        for state in &["frozen", "deverified", "unverified", ""] {
            let raw = format!(r#"{{"id":1,"verification":"{state}"}}"#);
            let o: Offer = serde_json::from_str(&raw).unwrap();
            assert!(!o.verified, "{state:?} must NOT parse as verified");
        }
    }

    #[test]
    fn offer_missing_verification_is_false() {
        let o: Offer = serde_json::from_str(r#"{"id":1}"#).unwrap();
        assert!(!o.verified);
    }
}
