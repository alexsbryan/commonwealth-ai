// SPDX-License-Identifier: AGPL-3.0-or-later
use arch_layers::{DepEdge, DepKind};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    fs,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub fn run(
    program: &Path,
    args: &[&str],
    snapshot: &Path,
    work: &Path,
    label: &str,
    timeout: Duration,
) -> Result<(i32, String, String), String> {
    if timeout.is_zero() {
        return Err(format!("{label}: timeout before spawn"));
    }
    fs::create_dir_all(work).map_err(|e| e.to_string())?;
    let stdout = work.join(format!("{label}.stdout"));
    let stderr = work.join(format!("{label}.stderr"));
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(snapshot)
        .env("CARGO_TARGET_DIR", work.join("target"))
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .stdin(Stdio::null())
        .stdout(fs::File::create(&stdout).map_err(|e| e.to_string())?)
        .stderr(fs::File::create(&stderr).map_err(|e| e.to_string())?);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd.spawn().map_err(|e| format!("{label}: spawn: {e}"))?;
    let began = Instant::now();
    let status = loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) => break status,
            None if began.elapsed() >= timeout => {
                #[cfg(unix)]
                {
                    let _ = Command::new("kill")
                        .args(["-KILL", "--", &format!("-{}", child.id())])
                        .status();
                }
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{label}: timeout after {}ms", timeout.as_millis()));
            }
            None => thread::sleep(Duration::from_millis(10)),
        }
    };
    let exit = status
        .code()
        .ok_or_else(|| format!("{label}: checker terminated abnormally ({status})"))?;
    Ok((
        exit,
        fs::read_to_string(stdout).map_err(|e| e.to_string())?,
        fs::read_to_string(stderr).map_err(|e| e.to_string())?,
    ))
}

pub fn edges(metadata: &str, snapshot: &Path) -> Result<Vec<DepEdge>, String> {
    let v: Value = serde_json::from_str(metadata).map_err(|e| format!("metadata JSON: {e}"))?;
    let packages = v["packages"].as_array().ok_or("metadata has no packages")?;
    let names: BTreeSet<&str> = packages.iter().filter_map(|p| p["name"].as_str()).collect();
    if names != super::fixture::PACKAGES.into_iter().collect() || packages.len() != names.len() {
        return Err("metadata does not contain exactly the four fixture packages".into());
    }
    let members: BTreeSet<&str> = v["workspace_members"]
        .as_array()
        .ok_or("metadata has no members")?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let ids: BTreeSet<&str> = packages.iter().filter_map(|p| p["id"].as_str()).collect();
    if members != ids || ids.len() != 4 {
        return Err("incomplete workspace membership".into());
    }
    let mut edges = Vec::new();
    for p in packages {
        let from = p["name"].as_str().ok_or("package without name")?;
        for dep in p["dependencies"]
            .as_array()
            .ok_or("package without dependency list")?
        {
            // Cargo's `name` is the actual package; `rename` is only its alias.
            let to = dep["name"]
                .as_str()
                .ok_or("dependency without actual name")?;
            if !names.contains(to) {
                return Err(format!("unexpected dependency {to}"));
            }
            let path = dep["path"].as_str().ok_or("non-local fixture dependency")?;
            if fs::canonicalize(path).map_err(|e| e.to_string())?
                != fs::canonicalize(snapshot.join(to)).map_err(|e| e.to_string())?
            {
                return Err(format!(
                    "dependency {to} resolves outside its pinned fixture path"
                ));
            }
            let kind = match dep["kind"].as_str() {
                None if dep["kind"].is_null() => DepKind::Normal,
                Some("build") => DepKind::Build,
                Some("dev") => DepKind::Dev,
                _ => return Err("unknown dependency kind".into()),
            };
            edges.push(DepEdge {
                from: from.into(),
                to: to.into(),
                kind,
                optional: dep["optional"].as_bool().ok_or("missing optional flag")?,
            });
        }
    }
    edges.sort_by_key(|e| (e.from.clone(), e.to.clone(), format!("{:?}", e.kind)));
    Ok(edges)
}

pub fn render_edges(edges: &[DepEdge]) -> Value {
    json!(edges
        .iter()
        .map(
            |e| json!({"from":e.from,"to":e.to,"kind":format!("{:?}",e.kind),"optional":e.optional})
        )
        .collect::<Vec<_>>())
}
