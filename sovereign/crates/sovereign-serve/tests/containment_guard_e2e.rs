// SPDX-License-Identifier: AGPL-3.0-or-later
//! The distributed-primary containment guard refuses boot in serve's own
//! binary too: `sovereign_serve::assemble` reaches the same
//! `ReloadFactory::build` the stock binary does, before serve binds its
//! listener. The stock case and the hazard are in sovereign-stock's
//! `containment_guard_e2e.rs` (pb-distribution-o3-tests).

use std::io::Read;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_sovereign-serve");

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("ephemeral port")
        .port()
}

#[test]
fn serve_alone_refuses_a_host_with_an_in_process_distributed_primary() {
    let root = tempfile::tempdir().expect("tempdir");
    let (listen, dead_rails) = (free_port(), free_port());
    // A declared host, a distributable primary, no compute-child boundary;
    // GGUFs that do not exist, so reaching a load is itself a failure.
    std::fs::write(
        root.path().join("config.toml"),
        format!(
            "[models]\nprimary = \"/nonexistent/Qwen3.5-122B-A10B-UD-Q5_K_XL-00001-of-00003.gguf\"\n\
             fast = \"/nonexistent/Qwen3.5-0.8B-UD-Q6_K_XL.gguf\"\n\
             embed = \"/nonexistent/Qwen3-Embedding-0.6B-Q8_0.gguf\"\n\n\
             [shared_model]\nrole = \"host\"\n\n\
             [compute]\nenabled = false\ndistributed_primary = false\n\n\
             [daemon]\nrails_base = \"http://127.0.0.1:{dead_rails}\"\n"
        ),
    )
    .expect("config");

    let mut child = Command::new(BIN)
        .arg("--data-dir")
        .arg(root.path())
        .args(["--listen", &format!("127.0.0.1:{listen}")])
        .env("HOME", root.path())
        .env("SOVEREIGN_SKIP_VRAM_CHECK", "1")
        .env_remove("SOVEREIGN_ALLOW_INPROCESS_DISTRIBUTED_PRIMARY")
        .env_remove("SOVEREIGN_RPC_DISCOVER")
        .env_remove("RUST_LOG")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn sovereign-serve");
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let t_out = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    let t_err = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });

    // Bounded: a guard that fails to fire is killed, never left listening.
    let addr = SocketAddr::from(([127, 0, 0, 1], listen));
    let mut answered = false;
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        answered |= TcpStream::connect_timeout(&addr, Duration::from_millis(20)).is_ok();
        match child.try_wait().expect("try_wait") {
            Some(s) => break Some(s),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    };
    let out = format!(
        "{}{}",
        t_out.join().unwrap_or_default(),
        t_err.join().unwrap_or_default()
    );
    let code = status.and_then(|s| s.code());

    assert!(
        matches!(code, Some(c) if c != 0),
        "serve must refuse this configuration with a non-zero exit, got {code:?}. Output:\n{out}"
    );
    for expected in [
        "HOST",
        "IN-PROCESS",
        "[compute]",
        "distributed_primary = true",
        "role = \"consumer\"",
    ] {
        assert!(
            out.contains(expected),
            "refusal message must contain {expected:?}. Output:\n{out}"
        );
    }
    assert!(
        !out.contains("does not exist"),
        "the guard must fire BEFORE model loading. Output:\n{out}"
    );
    assert!(!answered, "a refused serve bound :{listen}. Output:\n{out}");
}
