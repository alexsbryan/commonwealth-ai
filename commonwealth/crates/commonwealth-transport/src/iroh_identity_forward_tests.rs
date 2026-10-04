// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn head_of(target: &str) -> Vec<u8> {
    format!("GET {target} HTTP/1.1\r\nHost: x\r\nAccept: */*\r\n\r\n").into_bytes()
}

fn split(target: &str) -> Option<(String, String)> {
    split_app_name(&head_of(target)).map(|(n, h)| (n, String::from_utf8(h).unwrap()))
}

#[test]
fn the_first_path_segment_names_the_app_and_leaves_the_rest_intact() {
    let (name, head) = split("/chores/tasks?due=today").unwrap();
    assert_eq!(name, "chores");
    assert!(
        head.starts_with("GET /tasks?due=today HTTP/1.1\r\n"),
        "{head}"
    );
    // Everything after the request line is copied byte for byte.
    assert!(head.ends_with("Host: x\r\nAccept: */*\r\n\r\n"), "{head}");
}

/// A bare app root must reach the origin as `/`, not as an empty target —
/// an empty request target is not a valid HTTP/1.1 request line, and the
/// origin would answer 400 to what a person typed as a working URL.
#[test]
fn an_app_root_becomes_a_slash() {
    assert_eq!(split("/chores").unwrap().1.split(' ').nth(1), Some("/"));
    assert_eq!(split("/chores/").unwrap().1.split(' ').nth(1), Some("/"));
    assert_eq!(
        split("/chores?x=1").unwrap().1.split(' ').nth(1),
        Some("/?x=1")
    );
}

/// The failing inputs the character rule exists for. A name that could
/// express traversal, an encoded slash, or a stray byte never becomes a
/// lookup key at all — refused before the map is consulted (ARCH §10).
#[test]
fn a_name_that_could_express_traversal_is_refused_not_sanitized() {
    assert!(split("/../secrets").is_none());
    assert!(split("/..%2fsecrets").is_none());
    assert!(split("/cho res/x").is_none());
    assert!(split("/chores\u{7f}/x").is_none());
}

/// No name to bind: a bare root, an absolute-form target (which a bridge
/// client does not send), and a request line that is not three parts.
#[test]
fn a_head_with_no_usable_name_is_none() {
    assert!(split("/").is_none());
    assert!(split("http://elsewhere/chores/x").is_none());
    assert!(split_app_name(b"GET\r\n\r\n").is_none());
    assert!(split_app_name(b"").is_none());
}

/// The split runs BEFORE `rewrite_head`, so a client that names an app
/// and also forges an identity gets both handled: the app resolves, and
/// the forged header is still stripped.
#[test]
fn naming_an_app_does_not_let_a_forged_identity_through() {
    let raw = b"GET /chores/x HTTP/1.1\r\nX-Mesh-Member: Mallory\r\n\r\n";
    let (name, head) = split_app_name(raw).unwrap();
    assert_eq!(name, "chores");
    let (out, stripped) = rewrite_head(&head, &identity());
    assert_eq!(stripped, 1);
    let text = String::from_utf8(out).unwrap();
    assert!(!text.contains("Mallory"), "{text}");
    assert!(text.contains("X-Mesh-Member: LittleMac"), "{text}");
}

fn identity() -> Vec<(String, String)> {
    vec![
        ("X-Mesh-Member".into(), "LittleMac".into()),
        ("X-Mesh-Node".into(), "node-0b0b".into()),
    ]
}

fn header_values<'a>(head: &'a str, name: &str) -> Vec<&'a str> {
    head.lines()
        .filter_map(|l| l.split_once(':'))
        .filter(|(n, _)| n.trim().eq_ignore_ascii_case(name))
        .map(|(_, v)| v.trim())
        .collect()
}

/// THE failing input: a client that types the identity header itself.
/// Exactly one `X-Mesh-Member` reaches the origin, and it is the
/// acceptor's, whatever the client sent and however it cased the name.
#[test]
fn a_forged_identity_header_is_replaced_by_the_verified_one() {
    let head = b"GET /whoami HTTP/1.1\r\nHost: x\r\nx-mesh-member: forged\r\nX-MESH-NODE: node-forged\r\nX-Mesh-Anything: nope\r\n\r\n";
    let (out, stripped) = rewrite_head(head, &identity());
    let out = String::from_utf8(out).unwrap();
    assert_eq!(stripped, 3);
    assert_eq!(header_values(&out, "x-mesh-member"), vec!["LittleMac"]);
    assert_eq!(header_values(&out, "x-mesh-node"), vec!["node-0b0b"]);
    assert!(header_values(&out, "x-mesh-anything").is_empty());
    assert!(out.ends_with("\r\n\r\n"));
}

/// The request line and every other header pass byte for byte — `Range`
/// is a seek and the splice must not touch it.
#[test]
fn the_request_line_and_other_headers_pass_byte_exact() {
    let head = b"GET /library/title.bin HTTP/1.1\r\nHost: 127.0.0.1:1\r\nRange: bytes=10-19\r\nAccept: */*\r\n\r\n";
    let (out, stripped) = rewrite_head(head, &identity());
    assert_eq!(stripped, 0);
    let out = String::from_utf8(out).unwrap();
    assert!(out.starts_with("GET /library/title.bin HTTP/1.1\r\nHost: 127.0.0.1:1\r\nRange: bytes=10-19\r\nAccept: */*\r\n"));
    assert_eq!(header_values(&out, "x-mesh-member"), vec!["LittleMac"]);
}

/// A member name cannot end a line: control characters are dropped from
/// the value, so a name like `"Mac\r\nX-Admin: yes"` injects nothing.
#[test]
fn a_header_value_cannot_smuggle_a_second_header() {
    let hostile = vec![(
        "X-Mesh-Member".to_string(),
        "Mac\r\nX-Admin: yes".to_string(),
    )];
    let (out, _) = rewrite_head(b"GET / HTTP/1.1\r\n\r\n", &hostile);
    let out = String::from_utf8(out).unwrap();
    assert_eq!(
        header_values(&out, "x-mesh-member"),
        vec!["MacX-Admin: yes"]
    );
    assert!(header_values(&out, "x-admin").is_empty());
}

/// The failing input hm-1 exists for. A publisher declares its own
/// credential for its own origin; a viewer sends the same header name
/// with a value of its choosing. Append-only would put the viewer's copy
/// first and let the origin pick — so the declared one must DISPLACE it,
/// not merely follow it.
#[test]
fn a_declared_header_displaces_the_clients_copy_of_that_name() {
    let declared = vec![("X-Emby-Token".to_string(), "the-holders-key".to_string())];
    let (out, stripped) = rewrite_head(
        b"GET /Items HTTP/1.1\r\nHost: h\r\nX-Emby-Token: the-viewers-key\r\n\r\n",
        &declared,
    );
    let out = String::from_utf8(out).unwrap();
    assert_eq!(
        header_values(&out, "x-emby-token"),
        vec!["the-holders-key"],
        "exactly one token on the wire, and it is the holder's"
    );
    assert_eq!(stripped, 1, "the client's attempt is counted, not silent");
    assert!(out.contains("Host: h"), "unrelated headers still pass");
}

/// Case and padding are the obvious ways around a naive comparison, and
/// `wire_value` erases a third: a name carrying bytes the wire drops.
#[test]
fn the_displacement_survives_case_padding_and_unwriteable_bytes() {
    let declared = vec![("Authorization".to_string(), "holder".to_string())];
    let (out, stripped) = rewrite_head(
        "GET / HTTP/1.1\r\nAUTHORIZATION: viewer-upper\r\n  authorization  : viewer-pad\r\nAuthoriz\u{7f}ation: viewer-ctl\r\n\r\n".as_bytes(),
        &declared,
    );
    let out = String::from_utf8(out).unwrap();
    assert_eq!(header_values(&out, "authorization"), vec!["holder"]);
    assert_eq!(stripped, 3, "all three client spellings are displaced");
}

/// A credential is not always one word. Jellyfin 12 authenticates only
/// `Authorization: MediaBrowser Token="<key>"` -- probed 2026-09-12
/// against a live 12.0.0, where `X-Emby-Token`, `X-MediaBrowser-Token` and
/// `?api_key=` each returned 401 on `/Items` and this returned 200 -- so
/// the inner space and the quotes are load-bearing bytes, not formatting.
/// `wire_value` keeps `' '..='~'` and trims only the ends, which is what
/// makes that true; a filter on `is_ascii_graphic` would silently drop the
/// space and hand the origin a credential it refuses.
#[test]
fn a_multi_word_declared_credential_reaches_the_origin_verbatim() {
    let credential = r#"MediaBrowser Token="a-key-1234""#;
    let declared = vec![("Authorization".to_string(), credential.to_string())];
    let (out, stripped) = rewrite_head(
        b"GET /Items HTTP/1.1\r\nHost: h\r\nAuthorization: MediaBrowser Token=\"the-viewers\"\r\n\r\n",
        &declared,
    );
    let out = String::from_utf8(out).unwrap();
    assert_eq!(
        header_values(&out, "authorization"),
        vec![credential],
        "the space and both quotes survive, and the viewer's copy does not"
    );
    assert_eq!(stripped, 1);
}

/// The strip must not become a general-purpose header eater: a name the
/// publisher did NOT declare is the client's business and passes through.
#[test]
fn an_undeclared_client_header_is_untouched() {
    let declared = vec![("X-Emby-Token".to_string(), "k".to_string())];
    let (out, stripped) = rewrite_head(
        b"GET / HTTP/1.1\r\nRange: bytes=0-9\r\nCookie: c\r\n\r\n",
        &declared,
    );
    let out = String::from_utf8(out).unwrap();
    assert_eq!(header_values(&out, "range"), vec!["bytes=0-9"]);
    assert_eq!(header_values(&out, "cookie"), vec!["c"]);
    assert_eq!(stripped, 0);
}

/// One plain `Content-Length` is a body; no framing header is no body;
/// everything a forward and an origin could read two ways is refused.
#[test]
fn body_framing_reads_one_content_length_and_refuses_the_rest() {
    let framing =
        |headers: &str| body_framing(format!("POST / HTTP/1.1\r\n{headers}\r\n").as_bytes());
    assert_eq!(framing("Host: x\r\n"), BodyFraming::None);
    assert_eq!(framing("Content-Length: 12\r\n"), BodyFraming::Length(12));
    assert_eq!(
        framing("content-length:\t 12 \r\n"),
        BodyFraming::Length(12)
    );
    for refused in [
        "Transfer-Encoding: chunked\r\n",
        "Content-Length: 12\r\nTransfer-Encoding: chunked\r\n",
        "Transfer-Encoding: gzip\r\n",
        "TRANSFER-ENCODING: identity\r\n",
        "Transfer-Encoding\u{7f}: chunked\r\n",
        "Content-Length: 5\r\nContent-Length: 5\r\n",
        "Content-Length: 5, 5\r\n",
        "Content-Length: +5\r\n",
        "Content-Length: \r\n",
        "Content-Length: 99999999999999999999\r\n",
    ] {
        assert!(
            matches!(framing(refused), BodyFraming::Refused(_)),
            "{refused:?} must be refused, got {:?}",
            framing(refused)
        );
    }
}

/// THE failing input for the framing rule. A member opens with a chunked
/// request and puts a second request behind it naming another member's
/// key — or naming none, which the ring routes serve as a local process
/// (commonwealth-rails `ring_routes.rs` `roster_refusal`). Nothing after a
/// head this forward cannot frame may reach the origin, or the bytes
/// behind it are a request no rewrite touched.
#[tokio::test]
async fn a_request_behind_an_unframeable_one_never_reaches_the_origin() {
    let attacks: [&[u8]; 3] = [
        b"POST /internal/ring/sync HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\nGET /internal/ring/sync HTTP/1.1\r\nX-Mesh-Pubkey: victim\r\n\r\n",
        b"POST /internal/ring/sync HTTP/1.1\r\nContent-Length: 3\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\nGET /internal/ring/sync HTTP/1.1\r\n\r\n",
        b"POST /internal/ring/sync HTTP/1.1\r\nTransfer-Encoding: gzip, chunked\r\n\r\n0\r\n\r\nGET /internal/ring/sync HTTP/1.1\r\nX-Mesh-Pubkey: victim\r\n\r\n",
    ];
    let stamp = vec![("X-Mesh-Pubkey".to_string(), "dialer".to_string())];
    for attack in attacks {
        let mut reader = tokio::io::BufReader::new(std::io::Cursor::new(attack.to_vec()));
        let mut origin = Vec::new();
        let mut buf = Vec::new();
        assert!(read_head(&mut reader, &mut buf).await.unwrap());
        let flow = forward_request(&buf, &mut reader, &mut origin, &stamp).await;
        let seen = String::from_utf8_lossy(&origin);
        assert!(flow.is_break(), "the connection must end here: {seen}");
        assert!(
            !seen.contains("victim"),
            "a forged key reached the origin: {seen}"
        );
        assert!(
            !seen.contains("GET /internal/ring/sync"),
            "a request no rewrite touched reached the origin: {seen}"
        );
    }
}

/// Keep-alive: two requests on one connection, a body between them, and
/// the identity lands on BOTH heads while the body is copied untouched.
#[tokio::test]
async fn every_request_on_a_kept_alive_connection_carries_the_identity() {
    let input = b"POST /a HTTP/1.1\r\nContent-Length: 5\r\nX-Mesh-Member: forged\r\n\r\nhelloGET /b HTTP/1.1\r\n\r\n".to_vec();
    let mut reader = tokio::io::BufReader::new(std::io::Cursor::new(input));
    let mut out = Vec::new();
    let headers = identity();
    let mut buf = Vec::new();
    while read_head(&mut reader, &mut buf).await.unwrap() {
        let framing = body_framing(&buf);
        let (head, _) = rewrite_head(&buf, &headers);
        out.extend_from_slice(&head);
        if let BodyFraming::Length(n) = framing {
            let mut body = (&mut reader).take(n);
            tokio::io::copy(&mut body, &mut out).await.unwrap();
        }
    }
    let out = String::from_utf8(out).unwrap();
    assert_eq!(out.matches("X-Mesh-Member: LittleMac").count(), 2);
    assert!(out.contains("\r\n\r\nhelloGET /b HTTP/1.1\r\n"), "{out}");
    assert!(!out.contains("forged"));
}
