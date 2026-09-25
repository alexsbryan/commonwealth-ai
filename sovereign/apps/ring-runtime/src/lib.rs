//! The guest runtime — the wasm half of the page an iroh guest link opens.
//!
//! A browser cannot open a UDP socket, so it cannot speak iroh natively. This
//! module IS an iroh endpoint compiled to wasm (relay-only): the page dials the
//! wall's node by public key through a relay, on the `GUEST_ALPN` the daemon
//! already accepts, and speaks one HTTP/1.1 request over the resulting
//! bi-stream. The relay forwards ciphertext and nothing proxies HTTP — there is
//! no TLS to terminate and no door to expose (docs/RING_APP_LIBRARY.md, "The
//! page is an iroh endpoint"; docs/THE_LINK.md).
//!
//! Proven at these pins: `browser-dial` bd-1 measured every layer below
//! answering against a live node (`ralph/DECISIONS.md`, browser-dial-2):
//! `HTTP/1.1 200 OK` carrying a real rail row.
//!
//! Every layer is emitted to the page's `__trace` hook as it happens, never
//! only returned at the end — a layer that hangs must still be named, which is
//! the whole reason the trace exists (ARCH §1, glassbox).

use wasm_bindgen::prelude::*;

/// The ALPN the daemon routes to its guest-checking listener.
/// `commonwealth-transport/src/iroh.rs`: `GUEST_ALPN`.
pub const GUEST_ALPN: &[u8] = b"cwth/guest/0";

/// Push one layer line to the page's `__trace` collector, if the page defined
/// one. Best-effort: a missing collector must not break the dial.
fn emit(line: &str) {
    if let Ok(f) = js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("__trace")) {
        if let Ok(f) = f.dyn_into::<js_sys::Function>() {
            let _ = f.call1(&JsValue::UNDEFINED, &JsValue::from_str(line));
        }
    }
}

fn decode_hex_n32(s: &str) -> Result<[u8; 32], String> {
    let s = s.trim();
    if s.len() != 64 {
        return Err(format!("endpoint id is {} hex chars, expected 64", s.len()));
    }
    let mut out = [0u8; 32];
    for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
        let hi = (chunk[0] as char)
            .to_digit(16)
            .ok_or_else(|| format!("endpoint id is not hex at byte {i}"))?;
        let lo = (chunk[1] as char)
            .to_digit(16)
            .ok_or_else(|| format!("endpoint id is not hex at byte {i}"))?;
        out[i] = ((hi << 4) | lo) as u8;
    }
    Ok(out)
}

/// Dial the node named in `dial`, send `GET <path>` with
/// `Authorization: Bearer <token>` over one iroh bi-stream on `GUEST_ALPN`,
/// and return the full trace (every layer, plus the response verbatim).
///
/// `dial` is the `<hex-pubkey>@<relay-or-addr>[,…]` string a guest link's
/// `iroh=` fragment param carries; `token` is the grant bearer; `path` is the
/// grant-scoped route (e.g. `/v1/rail/log`).
#[wasm_bindgen]
pub async fn dial_guest(dial: String, token: String, path: String) -> String {
    let mut trace = String::new();
    macro_rules! t {
        ($($a:tt)*) => {{
            let line = format!($($a)*);
            emit(&line);
            trace.push_str(&line);
            trace.push('\n');
        }};
    }

    t!("LAYER page: wasm module loaded and dial_guest entered (iroh =1.0.2)");

    // ── LAYER 1: parse the dial string the link carries ────────────────────
    let (id_hex, targets) = match dial.split_once('@') {
        Some(x) => x,
        None => {
            t!("LAYER parse: REFUSED — dial string has no '@': {dial}");
            return trace;
        }
    };
    let id = match decode_hex_n32(id_hex) {
        Ok(b) => b,
        Err(e) => {
            t!("LAYER parse: REFUSED — {e}");
            return trace;
        }
    };
    let Some(relay) = targets
        .split(',')
        .map(str::trim)
        .find(|s| s.starts_with("https://") || s.starts_with("http://"))
        .map(str::to_string)
    else {
        t!("LAYER parse: REFUSED — dial string carries no relay URL: {targets}");
        return trace;
    };
    t!("LAYER parse: ok — node id {id_hex}, relay {relay}");

    let node_id = match iroh::PublicKey::from_bytes(&id) {
        Ok(p) => p,
        Err(e) => {
            t!("LAYER parse: REFUSED — node id is not a valid public key: {e}");
            return trace;
        }
    };
    let relay_url: iroh::RelayUrl = match relay.parse() {
        Ok(u) => u,
        Err(e) => {
            t!("LAYER parse: REFUSED — relay URL did not parse: {e}");
            return trace;
        }
    };

    // ── LAYER 2: wasm bindgen / iroh endpoint construction ─────────────────
    t!("LAYER wasm-bindgen/iroh-endpoint: binding a browser endpoint (relay-only)…");
    let ep = match iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::generate())
        .relay_mode(iroh::RelayMode::Default)
        .alpns(vec![GUEST_ALPN.to_vec()])
        .bind()
        .await
    {
        Ok(e) => e,
        Err(e) => {
            t!("LAYER wasm-bindgen/iroh-endpoint: REFUSED — endpoint bind failed: {e}");
            return trace;
        }
    };
    t!(
        "LAYER wasm-bindgen/iroh-endpoint: ok — local endpoint {} bound, relay mode default",
        ep.id()
    );

    // ── LAYER 3: relay connect ─────────────────────────────────────────────
    t!("LAYER relay-connect: waiting for the relay handshake (endpoint.online())…");
    ep.online().await;
    t!("LAYER relay-connect: ok — endpoint reports online (relay handshake done)");

    let addr =
        iroh::EndpointAddr::from_parts(node_id, [iroh::TransportAddr::Relay(relay_url.clone())]);
    t!("LAYER tls/alpn+relay-hop: dialing {node_id} on GUEST_ALPN through the relay…");
    let conn = match ep.connect(addr, GUEST_ALPN).await {
        Ok(c) => c,
        Err(e) => {
            t!("LAYER tls/alpn+relay-hop: REFUSED — connect() failed: {e}");
            return trace;
        }
    };
    t!("LAYER tls/alpn+relay-hop: ok — QUIC connection established to {node_id} on GUEST_ALPN");

    // ── LAYER 4: the HTTP request over one bi-stream ───────────────────────
    t!(
        "LAYER tls/alpn: negotiated ALPN = {:?}",
        String::from_utf8_lossy(conn.alpn())
    );
    let (mut send, mut recv) = match conn.open_bi().await {
        Ok(s) => s,
        Err(e) => {
            t!("LAYER open-bi: REFUSED — {e}");
            return trace;
        }
    };
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: sovereign-guest\r\nAuthorization: Bearer {token}\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    );
    if let Err(e) = send.write_all(req.as_bytes()).await {
        t!("LAYER request-write: REFUSED — {e}");
        return trace;
    }
    t!(
        "LAYER request-write: ok — {} bytes of HTTP/1.1 sent on the bi-stream (send half kept open until the response, as the native bridge does)",
        req.len()
    );

    let body = match recv.read_to_end(512 * 1024).await {
        Ok(b) => b,
        Err(e) => {
            t!("LAYER response-read: REFUSED — {e}");
            return trace;
        }
    };
    // Close our half now the exchange is done — the native bridge finishes the
    // send stream only after the local TCP side is finished, never before.
    let _ = send.finish();
    let text = String::from_utf8_lossy(&body).to_string();
    let status = text
        .lines()
        .next()
        .unwrap_or("<no status line>")
        .to_string();
    t!(
        "LAYER daemon-guest-admission: response received, {} bytes",
        body.len()
    );
    t!("RESPONSE-STATUS: {status}");
    t!("RESPONSE-BODY: {text}");

    ep.close().await;
    trace
}
