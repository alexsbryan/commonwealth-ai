// SPDX-License-Identifier: AGPL-3.0-or-later
//! `HELP_MESH` — the `svrn mesh` root help, extracted from `mesh_cmd.rs`
//! (ARCH §3.1: the file was past its ceiling, and the help tables are data —
//! the cleanest thing to move).

pub(crate) const HELP_MESH: sovereign_cli_shared::help::Help = sovereign_cli_shared::help::Help {
    command: "svrn mesh",
    summary: "Manage the local Commonwealth mesh (create / join / rotate / status).",
    sections: &[
        sovereign_cli_shared::help::HelpSection::Usage("svrn mesh <subcommand> [args]"),
        sovereign_cli_shared::help::HelpSection::Subcommands(&[
            (
                "create",
                "Promote the solo mesh to a joinable mesh; print invite",
            ),
            (
                "join <arg>",
                "Join an existing mesh (bare key, https url, or sovereign://)",
            ),
            (
                "rotate",
                "Generate a new shareable join key (invalidates the previous)",
            ),
            (
                "grant --model <id>",
                "Lend named models to a NON-member for a bounded window; prints a guest link",
            ),
            (
                "use <link>",
                "Accept a guest link — `svrn chat` then routes to the issuing node",
            ),
            (
                "status",
                "Show mesh members, hosted knowledge, loaded models",
            ),
            (
                "transport",
                "Show each peer's live iroh path (direct / relayed / mixed)",
            ),
            (
                "media <peer>",
                "Print a localhost URL that reaches a member's media server (Jellyfin) by mesh key — no VPN, no port forwarded",
            ),
            (
                "app [<peer>] [<name>]",
                "Reach a member's published app: no args lists the members publishing apps; `<peer> <name>` prints a URL and probes it",
            ),
            (
                "offers",
                "What the neighbours have for sale or lending — every neighbour a row, the ones that did not answer NAMED; --why adds who vouched for each",
            ),
            ("balance", "Show your contribution to the mesh"),
            ("list", "Show every mesh this node has joined; the active one is marked"),
            ("switch <mesh>", "Park the active mesh and bring another one up"),
            ("forget <mesh>", "Drop a parked mesh from this node"),
            (
                "forget-member <node>",
                "Retire one member row — the repair for an endpoint-key collision",
            ),
            ("leave", "Leave the current mesh"),
            ("logs", "Show mesh daemon logs"),
            (
                "fetch-model <name>",
                "Pull a GGUF from a mesh peer over the tailnet (no R2 credentials required)",
            ),
            (
                "warm-cache <gguf>",
                "Pre-seed the RPC tensor cache from a local GGUF (offline; later serves with zero weight transfer)",
            ),
            (
                "plan <gguf> --devices <gb,..>",
                "Dry-run the tensor split across a mesh — per-device fit + headroom, offline (no load)",
            ),
            (
                "bench",
                "Measure how fast the model you are running actually decodes, and record it for `plan`",
            ),
            (
                "check-invariants --nodes <a,b,..>",
                "Poll /v1/mesh/status across nodes and assert convergence/no-ghost/liveness (soak harness)",
            ),
            (
                "soak-gate <findings.jsonl>",
                "Gate mesh-soak SLIs (violation rate, load latency) against a committed baseline",
            ),
        ]),
        sovereign_cli_shared::help::HelpSection::Notes(
            "Run `svrn mesh <subcommand> --help` for subcommand-specific flags.",
        ),
    ],
};
