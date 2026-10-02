// SPDX-License-Identifier: AGPL-3.0-or-later
//! The mesh records pb-mesh-exit-core moved to oicp-types, pinned to the
//! bytes they put on the wire BEFORE the move (the fixture was captured, and
//! this test first passed, against their commonwealth-core and
//! commonwealth-state definitions). Each fixture entry must decode as its type
//! and re-encode to the same JSON: a renamed, dropped or newly-defaulted field,
//! or a changed enum tag, turns this red.

use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;

const FIXTURE: &str = include_str!("mesh_records_wire_fixture.json");

fn round_trips<T: Serialize + DeserializeOwned>(name: &str) {
    let fixture: Value = serde_json::from_str(FIXTURE).expect("fixture parses");
    let entries = fixture[name]
        .as_array()
        .unwrap_or_else(|| panic!("fixture has no `{name}` array"));
    assert!(!entries.is_empty(), "`{name}` has no samples");
    for (i, wire) in entries.iter().enumerate() {
        let value: T = serde_json::from_value(wire.clone())
            .unwrap_or_else(|e| panic!("{name}[{i}] does not decode: {e}"));
        let back = serde_json::to_value(&value).expect("re-encodes");
        assert_eq!(&back, wire, "{name}[{i}] re-encodes differently: {back}");
    }
}

#[test]
fn every_moved_mesh_record_round_trips_its_captured_wire() {
    use oicp_types::activity::{ActivityEvent, ActivitySummary};
    use oicp_types::capabilities::NodeCapabilities;
    use oicp_types::contributions::{LedgerEvent, NodeContributions};
    use oicp_types::inference_plan::InferencePlan;
    use oicp_types::model_catalog::{ModelArchitecture, ModelAvailability, ModelInfo};
    use oicp_types::peer_preference::PeerPreference;
    use oicp_types::work_queue::{
        CompleteOutcome, HandoffPhase, IngestionHandoff, KnowledgeShardAssignment, LeasedUnit,
        UnitStatus, WorkUnit,
    };

    round_trips::<NodeCapabilities>("NodeCapabilities");
    round_trips::<KnowledgeShardAssignment>("KnowledgeShardAssignment");
    round_trips::<IngestionHandoff>("IngestionHandoff");
    round_trips::<HandoffPhase>("HandoffPhase");
    round_trips::<WorkUnit>("WorkUnit");
    round_trips::<UnitStatus>("UnitStatus");
    round_trips::<CompleteOutcome>("CompleteOutcome");
    round_trips::<LeasedUnit>("LeasedUnit");
    round_trips::<ModelInfo>("ModelInfo");
    round_trips::<ModelArchitecture>("ModelArchitecture");
    round_trips::<ModelAvailability>("ModelAvailability");
    round_trips::<LedgerEvent>("LedgerEvent");
    round_trips::<NodeContributions>("NodeContributions");
    round_trips::<ActivityEvent>("ActivityEvent");
    round_trips::<ActivitySummary>("ActivitySummary");
    round_trips::<InferencePlan>("InferencePlan");
    round_trips::<PeerPreference>("PeerPreference");
    assert_eq!(
        oicp_types::work_queue::PROCESSED_SHARDS_APP_ID,
        "corpus-engine"
    );
}
