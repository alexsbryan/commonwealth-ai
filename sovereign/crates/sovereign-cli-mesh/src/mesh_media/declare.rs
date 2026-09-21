// ─── declare: this node's own credential for its own origin ─────────────────

/// `svrn mesh media declare <header>` — store the credential THIS node adds to
/// requests reaching ITS OWN media origin.
///
/// # Why the value comes from stdin and not from an argument
///
/// An argument is in `ps` while the process runs and in `~/.zsh_history`
/// forever. Piping it is the difference between a secret the operator chose to
/// store and a secret their shell also kept. The README this serves previously
/// said `printf '%s' key > file`, which has the same defect — this verb exists
/// partly to retire that instruction.
///
/// The value is never echoed, never logged, and never printed by `--list`;
/// whether one is SET is operational and is printed. Same discipline as
/// `PUT /v1/mcp/servers/{name}/token`.
pub(crate) fn cmd_media_declare(args: &[String]) -> i32 {
    use std::io::Read;

    let dir = commonwealth_media::dir_under(&sovereign_contracts::rebrand::svrnmesh_root());

    if sovereign_cli_shared::help::wants_help(args) {
        eprintln!("Usage: svrn mesh media declare <header-name>     # value on stdin");
        eprintln!("       svrn mesh media declare <header-name> --clear");
        eprintln!("       svrn mesh media declare --list");
        eprintln!();
        eprintln!("Store a credential this node adds to requests reaching ITS OWN origin —");
        eprintln!("so housemates reach your server without holding your API key. The value");
        eprintln!("is added on YOUR machine, after the caller is admitted as a member, and");
        eprintln!("displaces any copy of that header the caller sent.");
        eprintln!();
        // The NAME is whatever the origin behind this node reads, and this verb
        // has no opinion about which server that is — it stores a header. The
        // example is a shape, not a recommendation: name the header your own
        // server documents, and put in it exactly what that server expects.
        //
        // `%` is not a format escape in Rust; `%%s` here printed a literal `%%s`
        // and the help text shipped a printf that emitted `%s` instead of the
        // key (caught 2026-09-12 by rendering it, not by reading it).
        eprintln!("  printf '%s' \"$KEY\" | svrn mesh media declare x-api-key");
        eprintln!();
        eprintln!("<header-name> is whatever YOUR server reads, and the value is the whole");
        eprintln!("thing that header carries — some want a bare key, some want a scheme and");
        eprintln!("a quoted token. Check your server's own docs: the name is a filename");
        eprintln!("here, so when a server changes its scheme you rename a file rather than");
        eprintln!("wait for a release. (Jellyfin 12, for one, reads only");
        eprintln!("`authorization: MediaBrowser Token=\"<key>\"`.)");
        eprintln!();
        eprintln!("The value is read from stdin so it never lands in your shell history,");
        eprintln!("stored 0600 under the secrets dir, and never printed back. It is NOT in");
        eprintln!("config.toml on purpose: a secret there rides along with anything that is");
        eprintln!("shared, synced, backed up, or gossiped to a peer.");
        eprintln!();
        eprintln!("Run `svrn daemon stop && svrn daemon start` after changing a");
        eprintln!("declaration — NOT `reload`. The declarations are read once, while the");
        eprintln!("acceptor is built, so a reload cannot apply one and will tell you there");
        eprintln!("was nothing to do.");
        return 0;
    }

    if args.iter().any(|a| a == "--list") {
        let declared = commonwealth_media::read_declared_in(&dir);
        if declared.is_empty() {
            println!("No declarations. This node adds no credential of its own to requests");
            println!("reaching its origin. ({})", dir.display());
            return 0;
        }
        println!("Headers this node adds to requests reaching its own origin:");
        for (name, _) in &declared {
            // Names only. The value is the thing this store exists to keep.
            println!("  {name}  <set>");
        }
        return 0;
    }

    let Some(name) = args.iter().find(|a| !a.starts_with("--")) else {
        eprintln!("svrn mesh media declare: name a header, e.g. `x-emby-token`.");
        eprintln!("Run with --help for the whole shape.");
        return 2;
    };

    if !commonwealth_media::valid_header_name(&name.to_ascii_lowercase()) {
        eprintln!(
            "svrn mesh media declare: {name:?} is not a usable header name \
             (letters, digits, - and _ only, 64 chars max)."
        );
        return 2;
    }

    if args.iter().any(|a| a == "--clear") {
        return match commonwealth_media::write_declared_in(&dir, name, "") {
            Ok(()) => {
                println!(
                    "Cleared {}. Run `svrn daemon stop && svrn daemon start` to apply \
                     (a reload cannot: declarations are read once, at acceptor build).",
                    name.to_ascii_lowercase()
                );
                0
            }
            Err(e) => {
                eprintln!("Could not clear {name}: {e}");
                1
            }
        };
    }

    if atty_stdin() {
        eprintln!("svrn mesh media declare: the value is read from STDIN, so it stays out of");
        eprintln!("your shell history. Pipe it:");
        eprintln!();
        eprintln!("  printf '%%s' \"$KEY\" | svrn mesh media declare {name}");
        return 2;
    }

    let mut value = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut value) {
        eprintln!("Could not read the value from stdin: {e}");
        return 1;
    }
    if value.trim().is_empty() {
        eprintln!("svrn mesh media declare: stdin was empty — nothing stored.");
        eprintln!("To remove a declaration, use --clear.");
        return 2;
    }

    match commonwealth_media::write_declared_in(&dir, name, &value) {
        Ok(()) => {
            // What is SET, never what it is.
            println!(
                "Stored {} (0600, {}). Run `svrn daemon stop && svrn daemon start` to \
                 apply — NOT `reload`, which is read once at acceptor build and will \
                 report nothing to do.",
                name.to_ascii_lowercase(),
                dir.display()
            );
            0
        }
        Err(e) => {
            eprintln!("Could not store {name}: {e}");
            1
        }
    }
}

#[cfg(unix)]
fn atty_stdin() -> bool {
    // SAFETY: `isatty` reads a descriptor's mode and has no side effects.
    unsafe { libc::isatty(libc::STDIN_FILENO) == 1 }
}

#[cfg(not(unix))]
fn atty_stdin() -> bool {
    false
}
