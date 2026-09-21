// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests for the setup-config surface: slot windows, models, node class, the
//! terminal/entry bindings and TOML roundtrip — see `setup_config.rs`.
//!
//! Their own file because keeping them inline put `setup_config.rs` past
//! its arch-gate slack, and this file past the 1,200-line ceiling
//! (ARCH §3.1/§3.2). `#[path]`, so the names are unchanged.

use super::*;

/// The whole point of the helper is that it does NOT bake in 9742. A
/// regression here restores the four-way-duplicated bug it replaced:
/// every internal-API caller silently pinned to the default port.
#[test]
fn internal_base_url_honours_a_moved_port() {
    assert_eq!(internal_daemon_base_for(9742), "http://127.0.0.1:9742");
    assert_eq!(internal_daemon_base_for(19742), "http://127.0.0.1:19742");
}

/// The loading path falls back to the compiled default rather than
/// erroring, matching the `#[serde(default)]` posture on the field: a
/// missing config means defaults. Asserted against the constant so moving
/// `default_internal_port` cannot leave the fallback behind.
#[test]
fn internal_base_url_falls_back_to_the_declared_default() {
    assert_eq!(
        internal_daemon_base_for(default_internal_port()),
        "http://127.0.0.1:9742"
    );
}

/// `ComputeSection`'s fields carry `#[serde(default)]` with no
/// `skip_serializing_if`, so `save_to` materializes `distributed_primary =
/// false` literally into every config it writes.
///
/// This is stated as a test because it KILLS an attractive design. It is
/// tempting to make the compute-child containment auto-arm — "default it on
/// when the node is a shared-model host, unless the operator explicitly said
/// false" — but with the flag written out explicitly there is no way to
/// distinguish "unset" from "deliberately false". An `Option<bool>`
/// migration would not fire on any existing config, including the one that
/// motivated it, and an auto-arm that ignores an explicit `false` is not a
/// default, it is an override of a stated choice. Containment is therefore
/// enforced by a boot guard that REFUSES and names the fix, not by silently
/// changing what the operator asked for.
#[test]
fn compute_section_is_serialized_explicitly_so_unset_is_not_recoverable() {
    let toml = toml::to_string_pretty(&ComputeSection::default()).expect("serialize");
    assert!(
        toml.contains("distributed_primary = false"),
        "expected an explicit `distributed_primary = false` in:\n{toml}"
    );
    assert!(
        toml.contains("enabled = false"),
        "expected an explicit `enabled = false` in:\n{toml}"
    );
}

/// The same rule, extended to the sub-table `[compute.work_offer]` — and
/// the reason it is a SECOND assertion rather than a line in the one
/// above: a nested table is where the temptation returns. `accept` could
/// have been an `Option<Vec<String>>` and `repos` a
/// `skip_serializing_if = "Vec::is_empty"`, and both would have made
/// "unset" recoverable in the one section where auto-arming means running
/// somebody else's argv.
///
/// It also pins the ORDER. `work_offer` is a table and `[compute]`'s two
/// booleans are scalars; TOML emits a table's scalars before its
/// sub-tables, so a field added after `work_offer` in the struct would be
/// re-parented INTO `[compute.work_offer]` on the next `save_to` with
/// nothing failing. Asserting the two booleans still parse back at the
/// top level is what catches that.
#[test]
fn the_work_offer_sub_table_is_serialized_explicitly_and_stays_a_sub_table() {
    let toml = toml::to_string_pretty(&ComputeSection::default()).expect("serialize");
    for key in [
        "kinds = []",
        "max_concurrent = 0",
        "yield_to_foreground = true",
        "accept = \"nobody\"",
        "accept_from = []",
    ] {
        assert!(
            toml.contains(key),
            "expected an explicit `{key}` in:\n{toml}"
        );
    }
    let back: ComputeSection = toml::from_str(&toml).expect("round-trip");
    assert!(!back.enabled, "[compute] enabled stayed at the top level");
    assert!(
        !back.distributed_primary,
        "[compute] distributed_primary stayed at the top level, not inside \
             [compute.work_offer]: {toml}"
    );
    assert!(back.work_offer.kinds.is_empty());
}

/// The zero value donates nothing, stated three ways because any ONE of
/// them is enough and a reader should not have to guess which.
#[test]
fn a_node_that_says_nothing_offers_nothing() {
    let section = WorkOfferSection::default();
    assert_eq!(section.accept, WorkAcceptFrom::Nobody);
    assert_eq!(section.max_concurrent, 0);
    assert_eq!(
        section
            .to_offer("linux", "x86_64", oicp_types::Isolation::Subprocess)
            .expect("an empty section is not an error"),
        None,
        "no kinds means no offer at all — not an offer of nothing"
    );
}

/// The tri-state survives the flat spelling. `Nobody` is `Some(∅)` and
/// `Anyone` is `None`; collapsing either into the other is the defect the
/// enum exists to prevent, and `accepts_from` reads them oppositely.
#[test]
fn the_accept_policy_carries_the_wire_tri_state() {
    let key = "3b6a27bcceb6a42d62a3a8d02a6f0d73653215771de243a63ac048a18b59da29";
    let mut section = WorkOfferSection {
        kinds: vec!["process:v1".to_string()],
        max_concurrent: 1,
        accept_from: vec![key.to_string()],
        ..Default::default()
    };

    let offer = |s: &WorkOfferSection| {
        s.to_offer("linux", "x86_64", oicp_types::Isolation::Subprocess)
            .expect("valid kind")
            .expect("kinds are set")
    };

    section.accept = WorkAcceptFrom::Nobody;
    let o = offer(&section);
    assert_eq!(o.accept_from, Some(Vec::new()));
    assert!(!o.accepts_from(key), "`nobody` accepts nobody");

    section.accept = WorkAcceptFrom::Listed;
    assert!(offer(&section).accepts_from(key));
    assert!(!offer(&section).accepts_from("someone-else"));

    section.accept = WorkAcceptFrom::Anyone;
    assert_eq!(offer(&section).accept_from, None);
    assert!(offer(&section).accepts_from("someone-else"));
}

/// The failing input: `process@1`, the spelling four parse-and-discard
/// helpers in this tree accept. A donor booting on it would offer a kind
/// no submitter can name.
#[test]
fn a_kind_that_is_not_id_vn_is_refused_naming_it() {
    let section = WorkOfferSection {
        kinds: vec!["process@1".to_string()],
        ..Default::default()
    };
    let err = section
        .to_offer("linux", "x86_64", oicp_types::Isolation::Subprocess)
        .expect_err("`process@1` is not a job kind");
    assert!(
        err.to_string().contains("process@1"),
        "the refusal must name the entry, got: {err}"
    );
}

/// THE NO-REGRESSION BAR for per-slot windows (2026-08-25).
///
/// Adding `fast_context_size` must be invisible to every config.toml
/// already on disk. An omitted key resolves to the primary's window, which
/// is exactly what the single global scalar did — so a host that does not
/// set it builds byte-identical contexts to the ones it built before the
/// key existed. If this ever fails, the change has stopped being additive
/// and every existing install's fast slot has silently been resized.
#[test]
fn an_unset_fast_window_is_the_primary_window() {
    let mut m = models("/p.gguf", None, "/e.gguf");
    m.fast_context_size = None;

    m.context_size = None; // and the default path too
    assert_eq!(m.effective_fast_context_size(), m.effective_context_size());

    for ctx in [4096, 16_384, 65_536] {
        m.context_size = Some(ctx);
        assert_eq!(m.effective_fast_context_size(), ctx);
        assert_eq!(m.effective_fast_context_size(), m.effective_context_size());
    }
}

/// The lever itself: when set, the fast slot's window is its own and the
/// primary's is untouched. Named failing input for the inverse defect —
/// wiring `fast_context_size` to BOTH slots would shrink the primary's
/// window on any host that set it, which is the more damaging mistake.
#[test]
fn a_set_fast_window_moves_only_the_fast_slot() {
    let mut m = models("/p.gguf", None, "/e.gguf");
    m.context_size = Some(65_536);
    m.fast_context_size = Some(8_192);

    assert_eq!(m.effective_fast_context_size(), 8_192);
    assert_eq!(
        m.effective_context_size(),
        65_536,
        "the primary keeps its window — this key sizes the fast slot only"
    );
}

fn models(primary: &str, fast: Option<&str>, embed: &str) -> ModelsSection {
    ModelsSection {
        primary: PathBuf::from(primary),
        fast: fast.map(PathBuf::from),
        embed: PathBuf::from(embed),
        code: None,
        context_size: None,
        fast_context_size: None,
        extra: BTreeMap::new(),
        max_extras_memory_gb: None,
        primary_pool: None,
        edit: None,
    }
}

#[test]
fn fast_path_returns_primary_when_fast_unset() {
    let m = models("/models/primary.gguf", None, "/models/embed.gguf");
    assert_eq!(m.fast_path(), Path::new("/models/primary.gguf"));
    assert!(!m.has_explicit_fast());
}

#[test]
fn fast_path_returns_explicit_fast_when_set() {
    let m = models(
        "/models/primary.gguf",
        Some("/models/fast.gguf"),
        "/models/embed.gguf",
    );
    assert_eq!(m.fast_path(), Path::new("/models/fast.gguf"));
    assert!(m.has_explicit_fast());
}

#[test]
fn parse_config_without_fast_field_succeeds() {
    // The pod entrypoint writes a `[models]` table with only
    // primary + embed when SINGLE_MODEL=primary is set. Before
    // this commit, deserializing that TOML failed with
    // "missing field `fast`" and killed every Vast.ai pod at
    // the daemon-launch stage. Lock the now-Optional behaviour
    // in so a future refactor can't silently reintroduce the
    // hard requirement.
    let toml = r#"
[models]
primary = "/models/primary.gguf"
embed = "/models/embed.gguf"

[daemon]
[data]
"#;
    let cfg: SetupConfig = toml::from_str(toml).unwrap();
    assert_eq!(
        cfg.models().unwrap().primary,
        PathBuf::from("/models/primary.gguf")
    );
    assert!(cfg.models().unwrap().fast.is_none());
    assert_eq!(
        cfg.models().unwrap().fast_path(),
        Path::new("/models/primary.gguf")
    );
}

#[test]
fn iroh_enabled_tristate_parses_all_legacy_forms() {
    // `enabled` went bool → Option<bool> (auto-enable on mesh
    // participation, 2026-07). Pre-existing configs wrote
    // `enabled = true` / `enabled = false`; most wrote nothing.
    // All three must parse, and absent must be None (= auto).
    let base = r#"
[models]
primary = "/m/p.gguf"
embed = "/m/e.gguf"
"#;
    let cfg: SetupConfig = toml::from_str(base).unwrap();
    assert_eq!(cfg.iroh.enabled, None);

    let on: SetupConfig = toml::from_str(&format!("{base}\n[iroh]\nenabled = true\n")).unwrap();
    assert_eq!(on.iroh.enabled, Some(true));

    let off: SetupConfig = toml::from_str(&format!("{base}\n[iroh]\nenabled = false\n")).unwrap();
    assert_eq!(off.iroh.enabled, Some(false));

    // And None round-trips as None (absent, not `enabled = false`).
    let out = toml::to_string_pretty(&cfg).unwrap();
    let reparsed: SetupConfig = toml::from_str(&out).unwrap();
    assert_eq!(reparsed.iroh.enabled, None);
}

#[test]
fn iroh_relay_urls_default_empty_and_parse() {
    let base = r#"
[models]
primary = "/m/p.gguf"
embed = "/m/e.gguf"
"#;
    // Absent = empty (n0 default), and omitted on re-serialize.
    let cfg: SetupConfig = toml::from_str(base).unwrap();
    assert!(cfg.iroh.relay_urls.is_empty());
    let out = toml::to_string_pretty(&cfg).unwrap();
    assert!(
        !out.contains("relay_urls"),
        "empty relay_urls must serialize as absent: {out}"
    );

    // A configured self-hosted relay fleet round-trips, and the
    // sovereignty `discovery` knob parses.
    let with_relays: SetupConfig = toml::from_str(&format!(
            "{base}\n[iroh]\nenabled = true\nrelay_urls = [\"https://relay.corp.example:443\"]\ndiscovery = \"none\"\n"
        ))
        .unwrap();
    assert_eq!(
        with_relays.iroh.relay_urls,
        vec!["https://relay.corp.example:443".to_string()]
    );
    assert_eq!(with_relays.iroh.discovery.as_deref(), Some("none"));
    // Absent discovery = None (n0 default).
    assert_eq!(cfg.iroh.discovery, None);
}

/// The three participant states are three values, and none of them is a
/// flavour of another (§18.2).
#[test]
fn node_class_is_derived_from_models_and_entry() {
    let mut cfg = SetupConfig::unconfigured();
    assert_eq!(cfg.node_class(), NodeClass::Unconfigured);

    cfg.node.entry = Some("http://halo:9741/v1".into());
    assert_eq!(cfg.node_class(), NodeClass::Terminal);

    cfg.models = Some(models("/m/p.gguf", None, "/m/e.gguf"));
    assert_eq!(
        cfg.node_class(),
        NodeClass::Holder,
        "holding weights makes a node a holder even with an entry configured — \
             the entry is then simply unused"
    );
}

/// A mesh identity is a binding in its own right — the one `svrn setup
/// --terminal <join-link>` writes, and the one ARCH §7.5 asks for.
#[test]
fn an_identity_alone_makes_a_terminal() {
    let mut cfg = SetupConfig::unconfigured();
    cfg.node.entry_node = Some("44ae76142b0c3c723051ff98f043104a".into());
    assert_eq!(cfg.node_class(), NodeClass::Terminal);
    assert_eq!(
        cfg.node.binding(),
        Some(EntryBinding::Node(
            "44ae76142b0c3c723051ff98f043104a".into()
        ))
    );
}

/// An address alone still binds — an entry node that is not a mesh member
/// has no identity to resolve, and that case stays supported.
#[test]
fn an_address_alone_still_binds() {
    let mut cfg = SetupConfig::unconfigured();
    cfg.node.entry = Some("http://halo:9741/v1".into());
    assert_eq!(
        cfg.node.binding(),
        Some(EntryBinding::Address("http://halo:9741/v1".into()))
    );
}

/// An empty string is not a binding. Serde `default` plus a hand-edited
/// file can produce one, and treating it as present would send every turn
/// to `"" `and report the node as a working terminal (§18.3).
#[test]
fn a_blank_binding_is_no_binding() {
    let mut cfg = SetupConfig::unconfigured();
    cfg.node.entry = Some("   ".into());
    cfg.node.entry_node = Some(String::new());
    assert_eq!(cfg.node.binding(), None);
    assert_eq!(cfg.node_class(), NodeClass::Unconfigured);
}

/// Two bindings is a file that says two different things about where a
/// turn goes. Refused at LOAD, so it cannot be resolved by precedence
/// somewhere further in and land turns on the wrong machine.
#[test]
fn a_config_carrying_both_bindings_is_refused_at_load() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        "[node]\n\
             entry = \"http://halo:9741/v1\"\n\
             entry_node = \"44ae76142b0c3c723051ff98f043104a\"\n\
             \n\
             [data]\n\
             dir = \"/tmp/x\"\n",
    )
    .expect("write");
    let err = SetupConfig::load_from(&path).expect_err("both bindings must be refused");
    assert!(err.contains("entry_node"), "got: {err}");
    assert!(err.contains("entry"), "got: {err}");
}

/// One binding loads fine — the guard above must not have made every
/// terminal config unloadable.
#[test]
fn a_config_with_one_binding_loads() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        "[node]\n\
             entry_node = \"44ae76142b0c3c723051ff98f043104a\"\n\
             \n\
             [data]\n\
             dir = \"/tmp/x\"\n",
    )
    .expect("write");
    let cfg = SetupConfig::load_from(&path).expect("one binding is a valid terminal");
    assert_eq!(cfg.node_class(), NodeClass::Terminal);
}

/// A `[models]` table that EXISTS but names nothing is not a holder.
///
/// The desktop wizard writes exactly this shape mid-flight
/// (`commands/config_setup.rs` — `Some(ModelsSection::default())` so a file
/// exists before the user has picked slots), and a truncated or
/// badly-merged file lands in it too. Judged on presence, such a node was a
/// `Holder` holding nothing and `models()` handed callers three empty
/// `PathBuf`s — the same ambiguity `Option<ModelsSection>` was introduced to
/// remove, surviving on the other side of the boundary.
#[test]
fn a_placeholder_models_table_is_unconfigured_not_a_holder() {
    let mut cfg = SetupConfig::unconfigured();
    cfg.models = Some(ModelsSection::default());

    assert_eq!(
        cfg.node_class(),
        NodeClass::Unconfigured,
        "a `[models]` section naming no primary holds no models, whatever \
             shape the file is in"
    );
    let err = cfg
        .models()
        .expect_err("a placeholder section must refuse, not hand back empty paths");
    assert!(
        err.contains("svrn setup"),
        "the refusal must name the fix, got: {err}"
    );
}

/// ...and it must still LOAD, or the wizard breaks mid-flight.
///
/// Loadability and class are deliberately different questions with
/// different implementations: `validate_class` asks whether the file is
/// coherent enough to open (presence), `node_class` asks what the node is
/// (content). Collapsing them would either break the desktop's mid-setup
/// write or resurrect the placeholder-as-holder bug above.
#[test]
fn a_placeholder_models_table_still_loads() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    std::fs::write(
        &path,
        "[models]\nprimary = \"\"\nembed = \"\"\n\n[daemon]\n[data]\n",
    )
    .unwrap();

    let cfg = SetupConfig::load_from(&path)
        .expect("the desktop wizard's mid-flight config must keep loading");
    assert_eq!(cfg.node_class(), NodeClass::Unconfigured);
}

/// "Which embed model" is TWO questions, and a terminal answers them
/// differently.
///
/// `local_embed_model_id` is the space this node's own text lands in — the
/// entry node's, because a terminal embeds over HTTP.
/// `advertised_embed_model_id` is what it offers PEERS, and must stay
/// `None`: the collaborative-ingestion planner filters candidates by exact
/// match on it, so advertising here partitions work onto a node that can
/// only proxy every chunk back to the machine the planner was spreading
/// load off. One accessor answering both is how the terminal came to
/// advertise its entry node's model as its own (§10.6).
#[test]
fn a_terminal_embeds_under_its_entry_node_but_advertises_nothing() {
    let mut cfg = SetupConfig::unconfigured();
    cfg.node.entry = Some("http://halo:9741/v1".into());
    cfg.node.entry_embed_model = Some("qwen3-embedding-0.6b".into());

    assert_eq!(
        cfg.local_embed_model_id().as_deref(),
        Some("qwen3-embedding-0.6b"),
        "a terminal's vectors land in its ENTRY node's space, and that is \
             the honest name for them"
    );
    assert_eq!(
        cfg.advertised_embed_model_id(),
        None,
        "a terminal holds no embed slot, so it offers peers none"
    );
}

/// A holder answers both questions with its own slot.
#[test]
fn a_holder_advertises_the_slot_it_embeds_with() {
    let mut cfg = SetupConfig::unconfigured();
    cfg.models = Some(models("/m/p.gguf", None, "/m/qwen3-embedding-0.6b.gguf"));

    assert_eq!(
        cfg.local_embed_model_id().as_deref(),
        Some("qwen3-embedding-0.6b")
    );
    assert_eq!(
        cfg.advertised_embed_model_id().as_deref(),
        Some("qwen3-embedding-0.6b"),
        "the two answers coincide on a holder — which is why keying both on \
             one accessor stayed invisible until a terminal existed"
    );
}

/// An entry node with no embed slot leaves the space UNNAMEABLE, and that
/// must not become a name.
///
/// `svrn setup --terminal` supports this state explicitly (it prints "no
/// embed slot" and records the absence). The value must stay `None` here so
/// the provider maps it to the trait's `"unknown"` sentinel; an empty
/// string would pass `memory.rs`'s `model_known` gate as a real model named
/// empty-string and get embeddings persisted under it (§18.3).
#[test]
fn a_terminal_whose_entry_has_no_embed_slot_names_no_model() {
    let mut cfg = SetupConfig::unconfigured();
    cfg.node.entry = Some("http://halo:9741/v1".into());

    assert_eq!(cfg.local_embed_model_id(), None);
    assert_eq!(cfg.advertised_embed_model_id(), None);
}

/// `[models]` going missing must not read as "I meant to be a terminal".
///
/// This is the guard that lets the section be optional at all: without it,
/// a bad merge or a half-written file silently reclassifies the node, and
/// the failure surfaces later as a turn with nowhere to go (§18.3).
#[test]
fn a_config_with_neither_models_nor_an_entry_is_refused_at_load() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    std::fs::write(&path, "[daemon]\n[data]\n").unwrap();

    let err = SetupConfig::load_from(&path)
        .expect_err("a config that can neither serve nor route must not load");
    assert!(
        err.contains("neither") && err.contains("svrn setup"),
        "the refusal must name both the cause and the fix, got: {err}"
    );
}

/// The terminal config is a real, loadable shape — not merely one the
/// validator tolerates.
#[test]
fn a_terminal_config_loads_and_reports_its_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    std::fs::write(
        &path,
        "[node]\nentry = \"http://halo:9741/v1\"\n\n[daemon]\n[data]\n",
    )
    .unwrap();

    let cfg = SetupConfig::load_from(&path).expect("a terminal config must load");
    assert_eq!(cfg.node_class(), NodeClass::Terminal);
    assert_eq!(cfg.node.entry.as_deref(), Some("http://halo:9741/v1"));
    assert!(cfg.models.is_none());
}

/// A terminal's refusal has to say WHICH absence this is, because the two
/// are fixed differently: route instead, versus run setup.
#[test]
fn the_two_absences_of_models_are_reported_apart() {
    let mut terminal = SetupConfig::unconfigured();
    terminal.node.entry = Some("http://halo:9741/v1".into());
    let terminal_err = terminal.models().unwrap_err();
    assert!(
        terminal_err.contains("terminal") && terminal_err.contains("http://halo:9741/v1"),
        "a terminal's refusal must name the class and the entry node, got: {terminal_err}"
    );

    let unconfigured_err = SetupConfig::unconfigured().models().unwrap_err();
    assert!(
        unconfigured_err.contains("svrn setup") && !unconfigured_err.contains("terminal —"),
        "an unconfigured node's refusal must point at setup, got: {unconfigured_err}"
    );
}

/// A terminal budgets against the documented default rather than reading a
/// slot it does not have — and says so instead of panicking.
#[test]
fn a_terminal_reports_the_default_context_window() {
    let mut cfg = SetupConfig::unconfigured();
    cfg.node.entry = Some("http://halo:9741/v1".into());
    assert_eq!(cfg.effective_context_size(), default_context_size());
    assert_eq!(cfg.primary_model_stem(), None);
    assert_eq!(cfg.embed_model_stem(), None);
}

#[test]
fn roundtrip_minimal_config() {
    let cfg = SetupConfig {
        engine: Default::default(),
        compute: Default::default(),
        search: Default::default(),
        models: Some(ModelsSection {
            primary: PathBuf::from("/models/primary.gguf"),
            fast: Some(PathBuf::from("/models/fast.gguf")),
            embed: PathBuf::from("/models/embed.gguf"),
            code: None,
            context_size: None,
            fast_context_size: None,
            extra: BTreeMap::new(),
            max_extras_memory_gb: None,
            primary_pool: None,
            edit: None,
        }),
        node: NodeSection::default(),
        daemon: DaemonSection::default(),
        data: DataSection::default(),
        watched_folders: WatchedFoldersSection::default(),
        memory: Default::default(),
        iroh: Default::default(),
        shared_model: Default::default(),
        discovery: Default::default(),
        mcp_servers: Vec::new(),
    };
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    cfg.save_to(&path).unwrap();
    let loaded = SetupConfig::load_from(&path).unwrap();
    assert_eq!(
        loaded.models().unwrap().primary,
        cfg.models().unwrap().primary
    );
    assert_eq!(loaded.daemon.client_port, 9741);
    assert_eq!(loaded.daemon.internal_port, 9742);
    assert!(loaded.daemon.autostart);
}

#[test]
fn roundtrip_preserves_mcp_servers() {
    // Guards the clobber fix: the typed `mcp_servers` field must survive a
    // save()/load() round-trip. An untyped sibling `[[mcp_servers]]` array
    // would be dropped by `toml::to_string_pretty`, silently losing the
    // user's servers on the next desktop config save.
    use crate::mcp_config::{McpAuthConfig, McpServerConfig, McpTransportConfig};
    let cfg = SetupConfig {
        engine: Default::default(),
        compute: Default::default(),
        search: Default::default(),
        models: Some(ModelsSection {
            primary: PathBuf::from("/m/p.gguf"),
            fast: None,
            embed: PathBuf::from("/m/e.gguf"),
            code: None,
            context_size: None,
            fast_context_size: None,
            extra: BTreeMap::new(),
            max_extras_memory_gb: None,
            primary_pool: None,
            edit: None,
        }),
        node: NodeSection::default(),
        daemon: DaemonSection::default(),
        data: DataSection::default(),
        watched_folders: WatchedFoldersSection::default(),
        memory: Default::default(),
        iroh: Default::default(),
        shared_model: Default::default(),
        discovery: Default::default(),
        mcp_servers: vec![McpServerConfig {
            name: "vision".into(),
            description: Some("Describe images".into()),
            enabled: true,
            transport: McpTransportConfig::Http {
                url: "https://vision.example/mcp".into(),
                auth: McpAuthConfig::None,
            },
            global: true,
        }],
    };
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    cfg.save_to(&path).unwrap();
    let loaded = SetupConfig::load_from(&path).unwrap();
    assert_eq!(
        loaded.mcp_servers.len(),
        1,
        "mcp_servers must survive save/load"
    );
    assert_eq!(loaded.mcp_servers[0].name, "vision");
    assert!(matches!(
        &loaded.mcp_servers[0].transport,
        McpTransportConfig::Http { url, .. } if url == "https://vision.example/mcp"
    ));

    // An older config with no [[mcp_servers]] loads as empty (serde default).
    let old = "[models]\nprimary = \"/m/p.gguf\"\nembed = \"/m/e.gguf\"\n";
    let parsed: SetupConfig = toml::from_str(old).unwrap();
    assert!(parsed.mcp_servers.is_empty());
}
