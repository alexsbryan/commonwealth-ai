// SPDX-License-Identifier: AGPL-3.0-or-later
//! The command surface: three verbs, hand-parsed, no framework.
//!
//! ```text
//! cw-rails join <invite> [--name N] [--data-dir D] [--config F]
//! cw-rails run [--data-dir D] [--config F]
//! cw-rails media [<peer>] [--fanout <path>] [--peers a,b] [--listen P]
//! ```
//!
//! `media` is a curl-thin client of this daemon's own loopback API and holds
//! no mesh state: it prints what `svrn mesh media` prints, from the same JSON
//! the same routes return, so a person moving between the two daemons reads
//! one output format. It exists because "is my rails daemon on the mesh, and
//! who is on it" should be one line, not a `curl … | jq` recipe.
//!
//! # Exit codes
//!
//! `0` it worked · `1` it ran and refused (bad invite, no mesh, a peer that
//! offers nothing) · `3` could-not-judge: a precondition of the run is absent
//! — no writable data dir, no runtime — so nothing was attempted and nothing
//! is claimed (ARCH §18.2).

use std::path::PathBuf;
use std::process::ExitCode;

use crate::config::Config;
use crate::{join, RailsDaemon, RailsNode, Refusal};

pub const USAGE: &str = "\
cw-rails — the minimal rails daemon: your address on the mesh, with media on it.

  cw-rails join <invite> [--name N] [--data-dir D] [--config F]
      Join a mesh by invite (a sovereign://join/… link with an iroh dial).
      Writes node_id and mesh.json, then exits.

  cw-rails run [--data-dir D] [--config F]
      Serve the loopback API and gossip. Refuses to start with no mesh.

  cw-rails media [<peer>] [--fanout <path>] [--peers a,b] [--listen P]
      Ask the running daemon: who offers a library, the URL for one, or the
      same request to all of them.

Data dir: --data-dir, else $CW_RAILS_DIR, else ~/.commonwealth-rails.
Config:   <data-dir>/rails.toml (absent is fine — it is all defaults).
Logging:  RUST_LOG, default `info`.";

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Join {
        invite: String,
        name: Option<String>,
    },
    Run,
    Media {
        peer: Option<String>,
        fanout: Option<String>,
        peers: Option<Vec<String>>,
        listen: Option<u16>,
    },
    Help,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Args {
    pub command: Command,
    pub data_dir: Option<PathBuf>,
    pub config: Option<PathBuf>,
}

impl Args {
    /// Parse `std::env::args()`. A wrong argument is an error string the
    /// caller prints, never a guess at what was meant.
    pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Args, String> {
        let mut it = args.into_iter().skip(1).peekable();
        let verb = it.next().unwrap_or_else(|| "help".to_string());
        let mut data_dir = None;
        let mut config = None;
        let mut name = None;
        let mut invite: Option<String> = None;
        let mut peer: Option<String> = None;
        let mut fanout = None;
        let mut peers = None;
        let mut listen = None;
        while let Some(arg) = it.next() {
            let mut value = |flag: &str| it.next().ok_or_else(|| format!("{flag} wants a value"));
            match arg.as_str() {
                "--data-dir" => data_dir = Some(PathBuf::from(value("--data-dir")?)),
                "--config" => config = Some(PathBuf::from(value("--config")?)),
                "--name" => name = Some(value("--name")?),
                "--fanout" => fanout = Some(value("--fanout")?),
                "--listen" => {
                    let raw = value("--listen")?;
                    listen = Some(
                        raw.parse::<u16>()
                            .map_err(|_| format!("--listen {raw} is not a port"))?,
                    );
                }
                "--peers" => {
                    peers = Some(
                        value("--peers")?
                            .split(',')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string)
                            .collect(),
                    );
                }
                "-h" | "--help" => {
                    return Ok(Args {
                        command: Command::Help,
                        data_dir,
                        config,
                    })
                }
                other if other.starts_with('-') => {
                    return Err(format!("unknown argument `{other}`"))
                }
                positional => match verb.as_str() {
                    "join" if invite.is_none() => invite = Some(positional.to_string()),
                    "media" if peer.is_none() => peer = Some(positional.to_string()),
                    _ => return Err(format!("unexpected argument `{positional}`")),
                },
            }
        }
        let command = match verb.as_str() {
            "join" => Command::Join {
                invite: invite.ok_or("join wants an invite: `cw-rails join <invite>`")?,
                name,
            },
            "run" => Command::Run,
            "media" => Command::Media {
                peer,
                fanout,
                peers,
                listen,
            },
            "help" | "-h" | "--help" => Command::Help,
            other => return Err(format!("unknown command `{other}`")),
        };
        Ok(Args {
            command,
            data_dir,
            config,
        })
    }
}

/// The whole program. Returns the process's exit code.
pub async fn main(args: Args) -> ExitCode {
    let data_dir = Config::resolve_data_dir(args.data_dir.as_deref());
    let config = match Config::load(&data_dir, args.config.as_deref()) {
        Ok(c) => c,
        Err(e) => return refuse(Refusal::Config(e)),
    };
    match args.command {
        Command::Help => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Command::Media {
            peer,
            fanout,
            peers,
            listen,
        } => media(listen.unwrap_or(config.listen), peer, fanout, peers).await,
        Command::Join { invite, name } => {
            let mut config = config;
            if let Some(n) = name {
                config.name = n;
            }
            let node = match RailsNode::bind(data_dir.clone(), config).await {
                Ok(n) => n,
                Err(e) => return refuse(e),
            };
            match join::join_and_persist(&node, &invite, &data_dir).await {
                Ok(joined) => {
                    println!(
                        "Joined {} as {} ({}).",
                        joined.mesh.name, node.config.name, joined.self_id
                    );
                    println!("{} member(s) on the roster.", joined.mesh.members.len());
                    println!("Now: cw-rails run");
                    ExitCode::SUCCESS
                }
                Err(e) => refuse(e),
            }
        }
        Command::Run => {
            let listen = config.listen;
            let node = match RailsNode::bind(data_dir.clone(), config).await {
                Ok(n) => n,
                Err(e) => return refuse(e),
            };
            let daemon = match RailsDaemon::start_from_disk(node).await {
                Ok(d) => d,
                Err(e) => return refuse(e),
            };
            eprintln!("cw-rails: http://127.0.0.1:{listen}/v1/mesh/status");
            match daemon.run().await {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => refuse(e),
            }
        }
    }
}

fn refuse(e: Refusal) -> ExitCode {
    eprintln!("cw-rails: {e}");
    ExitCode::from(e.exit_code())
}

/// `cw-rails media …` — a client of the loopback API, nothing more.
async fn media(
    listen: u16,
    peer: Option<String>,
    fanout: Option<String>,
    peers: Option<Vec<String>>,
) -> ExitCode {
    let base = format!("http://127.0.0.1:{listen}");
    let http = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("cw-rails media: no HTTP client: {e}");
            return ExitCode::from(3);
        }
    };
    if let Some(path) = fanout {
        return media_fanout(&http, &base, &path, peers).await;
    }
    let url = match &peer {
        Some(p) => format!("{base}/v1/mesh/media?peer={p}"),
        None => format!("{base}/v1/mesh/media"),
    };
    let Some(doc) = ask(&http, &url).await else {
        return ExitCode::from(3);
    };
    let (status, body) = doc;
    if !status.is_success() {
        eprintln!("cw-rails media: {}", refusal_text(&body, status));
        return ExitCode::from(1);
    }
    match peer {
        Some(name) => {
            // A URL is the ONE thing this verb exists to print, and a player
            // is pointed at whatever comes out. An empty line here would be
            // an absence defaulted into something that looks like an answer
            // (ARCH §18.3), so a 200 with no `url` is a refusal.
            let Some(url) = body["url"].as_str().filter(|u| !u.is_empty()) else {
                eprintln!(
                    "cw-rails media: the daemon answered 200 for '{name}' with no url — {body}"
                );
                return ExitCode::from(1);
            };
            println!("{url}");
            println!(
                "  peer  {} ({})",
                body["peer"].as_str().unwrap_or("?"),
                body["node_id"].as_str().unwrap_or("?")
            );
            println!("  path  {}", path_line(&body["path"]));
            ExitCode::SUCCESS
        }
        None => {
            let offering = body["offering"].as_array().cloned().unwrap_or_default();
            if offering.is_empty() {
                println!(
                    "No member offers a media origin. A holder declares one with \
                     `[media] origin = \"127.0.0.1:8096\"` in rails.toml and restarts; \
                     it shows here within one gossip round."
                );
                return ExitCode::SUCCESS;
            }
            println!("Members offering a media origin:");
            for o in &offering {
                let status =
                    format!("{}", o["status"].as_str().unwrap_or("?")).to_ascii_lowercase();
                println!(
                    "  {:<16} {}  {:<8} {}",
                    o["peer"].as_str().unwrap_or("?"),
                    o["node_id"].as_str().unwrap_or("?"),
                    status,
                    path_line(&o["path"])
                );
            }
            println!();
            println!("Play one:  cw-rails media <peer>");
            ExitCode::SUCCESS
        }
    }
}

async fn media_fanout(
    http: &reqwest::Client,
    base: &str,
    path: &str,
    peers: Option<Vec<String>>,
) -> ExitCode {
    let mut body = serde_json::json!({ "path": path });
    if let Some(p) = peers {
        body["peers"] = serde_json::json!(p);
    }
    let url = format!("{base}/v1/mesh/media/fanout");
    let response = match http.post(&url).json(&body).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("cw-rails media: daemon at {base} not reachable: {e}");
            return ExitCode::from(3);
        }
    };
    let status = response.status();
    let doc: serde_json::Value = match response.json().await {
        Ok(d) => d,
        Err(e) => {
            eprintln!("cw-rails media: response shape mismatch: {e}");
            return ExitCode::from(1);
        }
    };
    if !status.is_success() {
        eprintln!("cw-rails media fanout: {}", refusal_text(&doc, status));
        return ExitCode::from(1);
    }
    let rows = doc["rows"].as_array().cloned().unwrap_or_default();
    println!(
        "{} → {} member(s) asked",
        doc["path"].as_str().unwrap_or(path),
        doc["asked"].as_u64().unwrap_or(rows.len() as u64)
    );
    for r in &rows {
        println!(
            "  {:<16} {}  {:>5} ms  {}",
            r["name"].as_str().unwrap_or("?"),
            r["node_id"].as_str().unwrap_or("?"),
            r["elapsed_ms"].as_u64().unwrap_or(0),
            fanout_line(r)
        );
    }
    ExitCode::SUCCESS
}

/// What the daemon said when it refused. The routes answer
/// `{"error": "..."}`, and a body that does NOT is printed whole rather than
/// replaced by the word "refused" — a substitution there spends the reader's
/// attention on the wrong question (ARCH §18.3).
fn refusal_text(body: &serde_json::Value, status: reqwest::StatusCode) -> String {
    match body["error"].as_str() {
        Some(e) => e.to_string(),
        None => format!("HTTP {} with no `error` field: {body}", status.as_u16()),
    }
}

/// One row's verdict, rendered. Kept beside the fan-out printer so the three
/// verdicts stay exhaustive in one place.
fn fanout_line(r: &serde_json::Value) -> String {
    match r["verdict"].as_str().unwrap_or("?") {
        "served" => format!(
            "{} {}B{}{}",
            r["status"].as_u64().unwrap_or(0),
            r["bytes"].as_u64().unwrap_or(0),
            if r["truncated"].as_bool().unwrap_or(false) {
                " (truncated)"
            } else {
                ""
            },
            r["content_type"]
                .as_str()
                .map(|c| format!("  {c}"))
                .unwrap_or_default()
        ),
        "failed" => format!("failed — {}", r["reason"].as_str().unwrap_or("")),
        "never_asked" => format!("not asked — {}", r["reason"].as_str().unwrap_or("")),
        other => other.to_string(),
    }
}

/// The live path, or the honest absence of one.
fn path_line(p: &serde_json::Value) -> String {
    let Some(kind) = p["path"].as_str() else {
        return "no path yet (nothing dialed)".to_string();
    };
    match p["relay"].as_str() {
        Some(r) => format!("{kind} via {r}"),
        None => kind.to_string(),
    }
}

async fn ask(
    http: &reqwest::Client,
    url: &str,
) -> Option<(reqwest::StatusCode, serde_json::Value)> {
    match http.get(url).send().await {
        Ok(r) => {
            let status = r.status();
            match r.json::<serde_json::Value>().await {
                Ok(v) => Some((status, v)),
                Err(e) => {
                    eprintln!("cw-rails media: response shape mismatch: {e}");
                    None
                }
            }
        }
        Err(e) => {
            eprintln!("cw-rails media: daemon at {url} not reachable: {e} — is `cw-rails run` up?");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(s: &str) -> Vec<String> {
        std::iter::once("cw-rails".to_string())
            .chain(s.split_whitespace().map(str::to_string))
            .collect()
    }

    #[test]
    fn the_three_verbs_parse() {
        assert_eq!(
            Args::parse(argv("join sovereign://join/abc --name shim")).unwrap(),
            Args {
                command: Command::Join {
                    invite: "sovereign://join/abc".into(),
                    name: Some("shim".into())
                },
                data_dir: None,
                config: None,
            }
        );
        let run = Args::parse(argv("run --data-dir /var/rails")).unwrap();
        assert_eq!(run.command, Command::Run);
        assert_eq!(run.data_dir, Some(PathBuf::from("/var/rails")));
        assert_eq!(
            Args::parse(argv("media LittleMac")).unwrap().command,
            Command::Media {
                peer: Some("LittleMac".into()),
                fanout: None,
                peers: None,
                listen: None
            }
        );
        assert_eq!(
            Args::parse(argv("media --fanout /Items --peers a,b --listen 9750"))
                .unwrap()
                .command,
            Command::Media {
                peer: None,
                fanout: Some("/Items".into()),
                peers: Some(vec!["a".into(), "b".into()]),
                listen: Some(9750),
            }
        );
    }

    /// **The failing inputs.** Every one of these used to be a guess in some
    /// hand-rolled parser: a missing value swallowing the next flag, an
    /// unknown flag ignored, a verb typo running the default.
    #[test]
    fn a_wrong_argument_is_an_error_not_a_guess() {
        assert!(Args::parse(argv("join")).is_err(), "an invite is required");
        assert!(
            Args::parse(argv("run --data-dir")).is_err(),
            "flag with no value"
        );
        assert!(Args::parse(argv("run --forever")).is_err(), "unknown flag");
        assert!(Args::parse(argv("runn")).is_err(), "unknown verb");
        assert!(
            Args::parse(argv("media --listen not-a-port")).is_err(),
            "a port that is not a port"
        );
        assert!(
            Args::parse(argv("media one two")).is_err(),
            "a second positional is a typo, not a second peer"
        );
    }

    #[test]
    fn no_arguments_is_help_not_a_daemon() {
        assert_eq!(
            Args::parse(vec!["cw-rails".to_string()]).unwrap().command,
            Command::Help
        );
    }

    /// The fan-out printer is exhaustive over the three verdicts the route
    /// can return; a row that rendered blank would read as a member that
    /// answered nothing.
    #[test]
    fn every_fanout_verdict_renders_something() {
        let served = serde_json::json!({
            "verdict": "served", "status": 200, "bytes": 213,
            "content_type": "application/json", "truncated": false
        });
        assert!(fanout_line(&served).contains("200 213B"));
        assert!(fanout_line(&served).contains("application/json"));
        let cut = serde_json::json!({ "verdict": "served", "status": 200, "bytes": 1024, "truncated": true });
        assert!(fanout_line(&cut).contains("(truncated)"));
        let failed = serde_json::json!({ "verdict": "failed", "reason": "timed out" });
        assert_eq!(fanout_line(&failed), "failed — timed out");
        let never = serde_json::json!({ "verdict": "never_asked", "reason": "no member matching 'Nobody'" });
        assert!(fanout_line(&never).starts_with("not asked — no member"));
    }

    /// **The failing input.** A 200 whose body has no `error` used to print
    /// the bare word "refused", which reads as the daemon's word and is not.
    #[test]
    fn a_refusal_we_cannot_parse_is_printed_whole_not_replaced_by_a_word() {
        let spoken = serde_json::json!({"error": "no member matching 'Nobody'"});
        assert_eq!(
            refusal_text(&spoken, reqwest::StatusCode::NOT_FOUND),
            "no member matching 'Nobody'"
        );
        let mute = serde_json::json!({"detail": "something else entirely"});
        let text = refusal_text(&mute, reqwest::StatusCode::BAD_GATEWAY);
        assert!(text.contains("502"), "{text}");
        assert!(text.contains("something else entirely"), "{text}");
    }

    /// A member with no live path is a row that says so, not a blank column.
    #[test]
    fn a_member_with_no_path_renders_the_absence() {
        assert_eq!(
            path_line(&serde_json::Value::Null),
            "no path yet (nothing dialed)"
        );
        assert_eq!(
            path_line(&serde_json::json!({"path": "relayed", "relay": "https://usw1-1.relay/"})),
            "relayed via https://usw1-1.relay/"
        );
        assert_eq!(path_line(&serde_json::json!({"path": "direct"})), "direct");
    }
}
