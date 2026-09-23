// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn ring`'s usage text, split out of `mod.rs` when that file crossed its
//! size ceiling (ARCH §3.1: move what grew into a sibling, behaviour
//! preserved). The strings are the block as it stood, with `serve`'s own
//! syntax line added and its blurb shortened to one line — same text
//! otherwise, same stream.

/// Print `svrn ring`'s usage to stderr, the output `svrn ring` with no
/// arguments has always produced.
pub(super) fn print() {
    eprintln!(
        "usage:\n\
         \x20 svrn ring new <dir> [--name <title>]\n\
         \x20 svrn ring roster add <person> (--key <node-pubkey-hex> | --self) [--on <op-id>] --ring <ns>\n\
         \x20 svrn ring roster show --ring <ns>\n\
         \x20 svrn ring introduce <person> --key <node-pubkey-hex> --reason <why> --ring <ns>\n\
         \x20 svrn ring show <ns> [--dir <bundle-dir>] [--port <n>]\n\
         \x20 svrn ring host <ns> --dir <bundle-dir> [--bind <addr:port>] [--read]\n\
         \x20 svrn ring log <ns> [--json]\n\
         \x20 svrn ring checkpoint <ns> [--out <file>]\n\
         \x20 svrn ring checkpoint --verify <file> [--roster <file>]\n\
         \x20 svrn ring seal <ns>\n\n\
         new     scaffold a ring app (index.html, app.js, its reducer and its tests).\n\
         roster  bind a person's name to the node key they sign with, and show why\n\
         \x20       each key is here.\n\
         introduce\n\
         \x20       vouch for a key on the journal, so the row that admits it can name\n\
         \x20       the act instead of somebody's memory. It admits NOBODY by itself.\n\
         show    open the app on THIS machine at http://127.0.0.1:4318/.\n\
         host    declare an app to the ROOM and bind the guest door (--clear ends one).\n\
         \x20       The bind is the door's ONE address for every app — a wildcard on the\n\
         \x20       door's port unless --bind names one; the guest link's address is\n\
         \x20       derived per host, so a wildcard is never advertised.\n\
         log     the acts on this journal, in the order every node applies them,\n\
         \x20       and everything the rail could not account for.\n\
         checkpoint\n\
         \x20       the ring's record, frozen: the journal verbatim, the roster it\n\
         \x20       was admitted under, and the digest that vouches it is complete.\n\
         \x20       --verify runs the four steps over a frozen copy and refuses\n\
         \x20       every forgery by name; --roster verifies under your own roster.\n\
         seal    retire everything this node wrote before now, and delete it.\n\n\
         A ring namespace is created by its first write — there is nothing to\n\
         provision. Start with `roster add`, because an op signed by a key no\n\
         roster claims is a gap rather than an act.\n\n\
         What an act MEANS — a balance, a borrowed drill — is the app's, not\n\
         this CLI's. Open the app with `ring show` to see it rendered."
    );
}
