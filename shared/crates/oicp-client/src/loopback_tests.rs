use super::endpoint_is_loopback;

/// The forms a daemon endpoint actually takes on this fleet.
#[test]
fn loopback_endpoints_are_recognised() {
    for e in [
        "http://localhost:9741/v1",
        "http://127.0.0.1:9741/v1",
        "http://127.0.0.1:9741",
        "https://localhost:9741/v1",
        "http://[::1]:9741/v1",
        "http://127.9.9.9:9841/v1",
    ] {
        assert!(endpoint_is_loopback(e), "{e} is on this machine");
    }
}

/// A `terminal`'s entry node, and the shapes that must not be mistaken for
/// loopback. `localhost.example.com` is the one worth a test: a prefix or
/// `contains` check would call it local and let a `local_only` turn cross
/// the network.
#[test]
fn remote_endpoints_are_not_loopback() {
    for e in [
        "http://halo:9741/v1",
        "http://192.168.1.10:9741/v1",
        "http://localhost.example.com:9741/v1",
        "http://notlocalhost:9741/v1",
        "https://10.0.0.4:9741",
    ] {
        assert!(!endpoint_is_loopback(e), "{e} is another machine");
    }
}

/// Anything unparseable counts as OFF-box.
///
/// The asymmetry is deliberate (§18.3): a wrong "off-box" reading refuses a
/// turn that could have run, which the operator sees and can act on. A
/// wrong "on-box" reading carries a `local_only` prompt across the network,
/// which nobody sees at all.
#[test]
fn an_unreadable_endpoint_fails_toward_refusal() {
    for e in ["", "://", "http://", "garbage"] {
        assert!(
            !endpoint_is_loopback(e),
            "{e:?} cannot be shown to be local, so it must not be treated as local"
        );
    }
}
