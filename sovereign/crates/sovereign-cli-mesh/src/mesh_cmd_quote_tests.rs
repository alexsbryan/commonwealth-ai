// SPDX-License-Identifier: AGPL-3.0-or-later
//! `shell_quote` is judged by a real shell: the printed `svrn mesh join …`
//! line must hand the CLI the whole link. Unquoted, the `&` in a deep link
//! made zsh refuse the line ("parse error near `&'") and bash cut the link
//! at the first `&` — dropping the dial, so the join fell back to LAN
//! discovery and failed.

use super::shell_quote;

fn through_sh(quoted: &str) -> String {
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("printf %s {quoted}"))
        .output()
        .expect("sh runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn a_join_link_survives_the_shell_whole() {
    let link = "sovereign://join/cwth-5788-c57d-c16c?name=Meshsonics&iroh=46d0%40https%3A%2F%2Frelay.%2F%2C69.181.167.209%3A57530&exp=1790201724";
    assert_eq!(through_sh(&shell_quote(link)), link);
}

#[test]
fn an_embedded_single_quote_survives_too() {
    let s = "it's&a$link `x`";
    assert_eq!(through_sh(&shell_quote(s)), s);
}
