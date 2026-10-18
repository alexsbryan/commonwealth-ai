use super::*;

#[test]
fn v1_retired_reader_keys_are_known_ignored_and_never_persisted() {
    let declared = r#"
[[types]]
name = "case"
kind = "entity"
identity_criterion = "same case record"

[[types]]
name = "membership"
kind = "claim"
force = "assertive"
subject = "case"
"#;
    let registry = OntologyLanguageRegistry::builtin();
    let language = registry.get(1).unwrap();
    let plain: toml::Table = toml::from_str(declared).unwrap();
    let keyed: toml::Table = toml::from_str(&format!(
        "document_reading = false\ndocument_reader = \"one_shot\"\n{declared}"
    ))
    .unwrap();
    assert!(
        registry.unknown_keys(&keyed).is_empty(),
        "a retired key is named as retired, never as a typo"
    );
    assert_eq!(
        retired_keys(&keyed)
            .iter()
            .map(|(k, _)| *k)
            .collect::<Vec<_>>(),
        ["document_reading", "document_reader"]
    );
    let policies = language.parse(&keyed).unwrap();
    assert_eq!(policies, language.parse(&plain).unwrap());
    assert!(policies.reads_documents());
    let wire = serde_json::to_value(&policies).unwrap();
    assert!(wire.get("document_reading").is_none() && wire.get("document_reader").is_none());
}

#[test]
fn v1_document_reading_supports_a_commissive_metadata_subject() {
    let body: toml::Table = toml::from_str(
        r#"
[[types]]
name = "case"
kind = "entity"
identity_criterion = "the same issue"

[[types]]
name = "person"
kind = "entity"
attributes = [{ name = "email", type = "text" }, { name = "name", type = "text" }]
identity = ["email"]
source = { metadata = ["from", "to", "cc"], attributes = { email = "address", name = "display_name" } }

[[types]]
name = "stage_update"
kind = "claim"
force = "assertive"
subject = "case"

[[types]]
name = "commitment"
kind = "claim"
force = "commissive"
subject = "person"
"#,
    )
    .unwrap();
    let registry = OntologyLanguageRegistry::builtin();
    let policies = registry.get(1).unwrap().parse(&body).unwrap();
    let commitment = policies
        .shape
        .types
        .iter()
        .find(|ty| ty.name == "commitment")
        .unwrap();
    assert_eq!(commitment.force, Some(Force::Commissive));
    assert!(commitment.is_document_reading_eligible(&policies.shape.types));
}

/// PRIMITIVES §0: an event identified by its criterion is a subject the reader
/// reads claims about, as a source-free entity is (RESOLVE decides both).
#[test]
fn v1_a_claim_about_an_event_with_a_criterion_is_read() {
    let body: toml::Table = toml::from_str(
        r#"
[[types]]
name = "happening"
kind = "event"
identity_criterion = "the same occurrence"

[[types]]
name = "report"
kind = "claim"
force = "assertive"
subject = "happening"
"#,
    )
    .unwrap();
    let policies = OntologyLanguageRegistry::builtin()
        .get(1)
        .unwrap()
        .parse(&body)
        .unwrap();
    let report = policies.type_decl("report").unwrap();
    assert!(report.is_document_reading_eligible(&policies.shape.types));
    assert!(policies.reads_documents());
}

#[test]
fn a_retired_type_key_is_named_ignored_and_never_persisted() {
    let declared = r#"
[[types]]
name = "case"
kind = "entity"
identity_criterion = "same case record"
identity_bar = 0.5

[[types]]
name = "membership"
kind = "claim"
force = "assertive"
subject = "case"
"#;
    let registry = OntologyLanguageRegistry::builtin();
    let language = registry.get(1).unwrap();
    let plain: toml::Table = toml::from_str(declared).unwrap();
    let keyed: toml::Table = toml::from_str(&declared.replace(
        "identity_bar = 0.5",
        "identity_bar = 0.5\nidentity_evidential = [{ evidence = \"model_choice\", right = 6, of = 19, measured_on = \"x\" }]",
    ))
    .unwrap();
    assert_eq!(
        retired_type_keys(&keyed),
        [(
            "case".to_string(),
            "identity_evidential",
            V1_RETIRED_TYPE_KEYS[0].1
        )]
    );
    assert!(retired_type_keys(&plain).is_empty());
    let policies = language.parse(&keyed).unwrap();
    assert_eq!(policies, language.parse(&plain).unwrap());
    let wire = serde_json::to_string(&policies).unwrap();
    assert!(!wire.contains("identity_evidential"), "{wire}");
}

/// A deal whose stage a protocol fold derives from one claim kind.
const DEALS: &str = r#"
[[types]]
name = "deal"
kind = "entity"
identity_criterion = "the same transaction"
attributes = [{ name = "stage", type = "text", values = ["open", "won", "lost"], derived = "deal_stage" }]

[[types]]
name = "stage_update"
kind = "claim"
force = "assertive"
subject = "deal"
attributes = [{ name = "stage", type = "text", values = ["open", "won", "lost"] }]

[[folds]]
id = "deal_stage"
by = "protocol"
from = ["^subject"]

[folds.protocol]
identity = "stage"

[[folds.protocol.rules]]
id = "open"
claim_kind = "stage_update"
state = "open"
when = { stage = "open" }

[[folds.protocol.rules]]
id = "won"
claim_kind = "stage_update"
state = "won"
when = { stage = "won" }
"#;

fn v1(body: &str) -> Result<OntologyPolicies> {
    OntologyLanguageRegistry::builtin()
        .get(1)
        .unwrap()
        .parse(&toml::from_str(body).unwrap())
}

fn rules(p: &OntologyPolicies) -> Vec<(String, String, usize)> {
    p.derivation.derived.folds[0]
        .protocol
        .as_ref()
        .unwrap()
        .rules
        .iter()
        .map(|r| (r.claim_kind.clone(), r.state.clone(), r.when.len()))
        .collect()
}

/// A protocol fold's record is given the method's two lifecycle kinds: one
/// replaced, a kind of the fold's claim kind so its rules give it a state read
/// from their values, and one done under it, which no rule moves. Without a
/// fold neither is supplied.
#[test]
fn a_protocol_folded_type_is_given_a_replaced_and_an_act_under_kind() {
    let p = v1(DEALS).unwrap();
    let replaced = p
        .type_decl("deal_replaced")
        .expect("deal_replaced supplied");
    assert_eq!(replaced.subject.as_deref(), Some("deal"));
    assert_eq!(replaced.specializes.as_deref(), Some("stage_update"));
    assert!(replaced.is_document_reading_eligible(&p.shape.types));
    let act = p
        .type_decl("deal_act_under")
        .expect("deal_act_under supplied");
    assert_eq!(act.subject.as_deref(), Some("deal"));
    assert_eq!(act.specializes, None);
    assert!(act.is_document_reading_eligible(&p.shape.types));
    assert_eq!(rules(&p).len(), 2, "no rule is added by default");
    let unfolded = DEALS
        .split("[[folds]]")
        .next()
        .unwrap()
        .replace(", derived = \"deal_stage\"", "");
    let p = v1(&unfolded).unwrap();
    assert!(p.type_decl("deal_replaced").is_none() && p.type_decl("deal_act_under").is_none());
}

/// The levers: `replaced` and `acts_under` name the state each moves its
/// record to, so the replaced kind asks for no stage of its own; a state the
/// derived attribute does not list is refused; `lifecycle = false` supplies
/// neither kind.
#[test]
fn the_lifecycle_levers_pin_a_state_and_turn_the_kinds_off() {
    let pinned = DEALS.replace(
        "identity = \"stage\"\n",
        "identity = \"stage\"\nreplaced = \"lost\"\nacts_under = \"won\"\n",
    );
    let p = v1(&pinned).unwrap();
    assert_eq!(p.type_decl("deal_replaced").unwrap().specializes, None);
    let r = rules(&p);
    assert!(
        r.contains(&("deal_replaced".into(), "lost".into(), 0)),
        "{r:?}"
    );
    assert!(
        r.contains(&("deal_act_under".into(), "won".into(), 0)),
        "{r:?}"
    );
    let err = v1(&DEALS.replace(
        "identity = \"stage\"\n",
        "identity = \"stage\"\nreplaced = \"closed\"\n",
    ))
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("closed") && err.contains("deal_stage"),
        "{err}"
    );
    let off = DEALS.replace(
        "identity = \"stage\"\n",
        "identity = \"stage\"\nlifecycle = false\n",
    );
    let p = v1(&off).unwrap();
    assert!(p.type_decl("deal_replaced").is_none() && p.type_decl("deal_act_under").is_none());
}

/// A recipe that declares a type of the supplied name keeps its own.
#[test]
fn a_declared_type_of_a_supplied_name_is_the_recipes() {
    let own = format!(
        "{DEALS}\n[[types]]\nname = \"deal_act_under\"\nkind = \"claim\"\nforce = \"commissive\"\nsubject = \"deal\"\n"
    );
    let p = v1(&own).unwrap();
    let decls: Vec<_> = p
        .shape
        .types
        .iter()
        .filter(|t| t.name == "deal_act_under")
        .collect();
    assert_eq!(decls.len(), 1);
    assert_eq!(decls[0].force, Some(Force::Commissive));
}

/// `window_chars` sizes the windows; absent, the shipped size stands; outside
/// the range it refuses at load.
#[test]
fn window_chars_is_a_lever_held_to_its_range() {
    let shape =
        |p: &OntologyPolicies| serde_json::to_value(&p.shape).unwrap()["window_chars"].clone();
    assert_eq!(
        shape(&v1(&format!("window_chars = 30000\n{DEALS}")).unwrap()),
        30000
    );
    assert!(shape(&v1(DEALS).unwrap()).is_null());
    let err = v1(&format!("window_chars = 50000\n{DEALS}"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("window_chars = 50000"), "{err}");
}

/// `recipe validate` holds the supplied kinds and the pinned levers' rules to
/// the fold's own reading: a kind that declares no identity field is
/// identified by the state it enters, and choosing it is its qualification.
#[test]
fn the_lifecycle_kinds_and_levers_validate_clean() {
    let pinned = DEALS.replace(
        "identity = \"stage\"\n",
        "identity = \"stage\"\nreplaced = \"lost\"\nacts_under = \"won\"\n",
    );
    for body in [DEALS.to_string(), pinned] {
        let v = crate::enrichment::ontology::validate_block(&super::super::OntologyBlock {
            version: 1,
            body: toml::from_str(&body).unwrap(),
        });
        let ours: Vec<_> = v
            .errors
            .iter()
            .filter(|e| e.contains("deal_replaced") || e.contains("deal_act_under"))
            .collect();
        assert!(ours.is_empty(), "{ours:?}");
        assert!(
            v.notes
                .iter()
                .any(|n| n.starts_with("lifecycle: `deal_replaced` ")),
            "{:?}",
            v.notes
        );
    }
}
