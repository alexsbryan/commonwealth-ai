# svrn mesh bench and svrn mesh plan MUST construct the same MeasurementKey for the same configuration, or every record written is unfindable…

`svrn mesh bench` and `svrn mesh plan` MUST construct the same MeasurementKey for the same configuration, or every record written is unfindable and the store grows forever while `mesh plan` reports "not measured" for ever. Four rules keep them in step; breaking any one is silent.

1. SHARDS ARE ONLY THE DEVICES THAT HOLD BLOCKS. plan filters its DeviceRows (`r.blocks.is_some() || r.holds_output`) in resolve_speed; bench drops zero-block workers in shards_from_placement. An idle peer changes nothing about how the model decodes, and bench — which reads what the daemon says is LOADED — has no idle device to contribute, so if plan included one the two could never meet.

2. THE DIGEST'S `mode` COMES FROM TOPOLOGY, NOT FROM THE DAEMON. `digest_mode(shards)` = "local" when <=1 shard else "distributed". The daemon's SlotPlacement.mode has FIVE values (local | distributed | child-distributed | stream-split | forming) and `mesh plan` has no daemon to ask, so passing the daemon's string through would make a child-distributed run unfindable by any plan. The daemon's own word is preserved in MeasurementRecord.placement_human so nothing is hidden.

3. `placement_digest(mode, total_blocks, shards)` TAKES n_layer FROM THE GGUF, not from placement.total_blocks. A plain local load reports total_blocks: 0 (it computes no block plan), so the range for a local shard is (0, n_layer-1) derived from the header — which is exactly what plan hashes.

4. BLOCK RANGES ARE CONTIGUOUS AND ASCENDING IN DEVICE ORDER: RPC workers first (in the daemon's listed order), host last. That is the order plan_shards_weighted is called with; bench reconstructs ranges by cumulative worker block counts with the host taking the tail. `shards_from_placement` refuses (Err) rather than guessing when the counts don't sum to the total or the total disagrees with the GGUF's n_layer.

Test coverage: mesh_bench/tests.rs `a_solo_bench_and_a_solo_plan_agree_on_the_digest` (constructs both sides and asserts digest equality), `a_different_split_of_the_same_model_digests_differently`, plus mesh_cmd plan_tests `a_peer_holding_no_blocks_does_not_enter_the_digest` and `an_idle_shard_would_change_the_digest_if_it_reached_it`. If you change either side's shard construction, those four are the ones that must still pass.

NOT YET CLOSED (week 3): a worker swapping a GPU of equal VRAM is invisible to the digest — the stated blind spot. Week 3 folds each worker's hw fingerprint in (MeshDevice.backend already carries the field, currently dead-code-warned).
