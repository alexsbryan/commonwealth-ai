# sovereign-mesh

What is left of the Commonwealth mesh integration layer: `MeshReplicatedKv`
(`src/peer_adapter.rs`), the mesh implementation of
`sovereign_contracts::peer::ReplicatedKv`, and the work-atlas replication test
that drives it (`tests/main/work_atlas_store.rs`).

pb-mesh-dissolve moved every other ability to its owner and deleted the rest:
the endpoint, ring round, self-heal and address discovery are
commonwealth-rails' and commonwealth-discovery's; the join and invite
vocabulary is mesh-join-vocab's; the guest dialer is mesh-reach's; the
scheduler simulator and its scoreboard tests are in sovereign-serving-host's
tests; the pod tests are sovereign-pods'; the CLI's pieces (the wall link, the
identity handover's reader of the daemon's old store, `dial_probe`) are
sovereign-cli-mesh's. The crate is deleted once the work-atlas test has a home.
