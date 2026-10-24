// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;

fn refusal(raw: &str) -> String {
    parse_bind(raw).expect_err(&format!("{raw} must be refused"))
}

#[test]
fn every_interface_is_refused_naming_the_key() {
    for raw in ["0.0.0.0:50052", "[::]:50052"] {
        let e = refusal(raw);
        assert!(e.contains("[compute] rpc_peer"), "{e}");
        assert!(e.contains("every interface"), "{e}");
    }
}

#[test]
fn loopback_is_refused_naming_the_key() {
    for raw in ["127.0.0.1:50052", "[::1]:50052"] {
        let e = refusal(raw);
        assert!(
            e.contains("[compute] rpc_peer") && e.contains("loopback"),
            "{e}"
        );
    }
}

#[test]
fn port_zero_and_a_hostname_are_refused() {
    assert!(refusal("192.168.1.20:0").contains("port 0"));
    assert!(refusal("peer.local:50052").contains("is not an ip:port"));
    assert!(refusal("192.168.1.20").contains("is not an ip:port"));
}

#[test]
fn a_lan_or_tunnel_address_is_taken() {
    for raw in [
        "192.168.1.20:50052",
        " 100.64.0.7:50052 ",
        "[fd7a:115c::7]:50052",
    ] {
        let addr = parse_bind(raw).unwrap_or_else(|e| panic!("{raw}: {e}"));
        assert_eq!(addr.port(), 50052);
    }
}

#[test]
fn unset_lends_nothing() {
    let config: SetupConfig = toml::from_str("[node]\n").expect("a config with no [compute]");
    assert_eq!(configured_bind(&config), Ok(None));
}
