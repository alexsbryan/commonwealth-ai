# VAST SSH: THE SERVER REJECTS OUR PUBLIC KEY BEFORE ANY SIGNATURE. Account-side; every CLI lever exhausted 2026-08-04. Total cost of…

VAST SSH: THE SERVER REJECTS OUR PUBLIC KEY BEFORE ANY SIGNATURE. Account-side; every CLI lever exhausted 2026-08-04. Total cost of establishing this: $0.22 (Vast invoices), 4 instances, all destroyed.

THE DECISIVE OBSERVATION. `ssh -v` shows:
  Offering public key: ... SHA256:qxVIAfL2VMWAQtLMeFAiG0SlzWeT+7Y+P/itFZRmZoE
  Authentications that can continue: publickey
  Permission denied (publickey).
The server rejects the OFFERED PUBLIC KEY — that exchange happens BEFORE the client proves possession, so nothing about the private key (including its passphrase) is in play. The server's authorized_keys simply does not contain this key.

REPRODUCED ACROSS: 4 instances, 3 distinct physical machines, 2 countries (Taiwan/Spain), 2 images (pytorch devel + runtime), 2 GPU classes (A100 SXM4, GTX 1060), proxy AND direct ports. It is not host-specific.

FALSIFIED — do NOT re-run these:
  - Wrong/mismatched local key. Pubkey file and Vast's registered key are the SAME fingerprint (SHA256:qxVIAf...). ssh-agent holds it (`ssh-add -l` confirms).
  - Private key passphrase. Irrelevant — rejection precedes the signature step (see above).
  - ssh offering the wrong identity. Reproduced with `-i` + `IdentitiesOnly=yes`.
  - Proxy vs direct. Both. NOTE `vastai ssh-url` returns EITHER depending on the instance; `ssh_host`/`ssh_port` from `show instance` is the proxy.
  - `--onstart-cmd 'sleep infinity'`. An instance with NO onstart failed identically.
  - Writing authorized_keys via onstart. Did not help. (And the "Welcome to vast.ai" banner does NOT prove a gateway — Vast bakes it into the container's own sshd. My earlier inference from it was wrong.)
  - Not waiting long enough. The clean test waited for actual_status==running AND ports mapped (120s) PLUS 45s of sshd margin. Still denied.
  - Stale key registration. Deleted 1147518, created 1178540 BEFORE creating the test instance. Still denied.
  - `vastai attach ssh <id> <pubkey>` -> {'success': True, 'msg': 'SSH key added to instance.'}. Still denied. On a fresh instance it answers "already associated", so that message is NEVER evidence the key works.

REMAINING LEAD, uncheckable from the CLI: the registered key has `"default": null`. `vastai update ssh-key` only replaces the key VALUE — there is no default-setting flag. If Vast only injects the DEFAULT key, that is the bug, and it is settable only in the web console.

OPERATOR ACTION: Vast web console -> Account -> SSH Keys. Confirm the key is present AND default; if the console lists a different key than `vastai show ssh-keys` does, that mismatch IS the answer. Account 531256 (<email>), credit ~$12.4. Verify with `ssh -i ~/.ssh/id_ed25519 -o IdentitiesOnly=yes -p <port> root@<ip> true` on ONE cheap instance before renting a GPU.

DEBUG CHEAP: there are 64 offers under $0.15/hr; a GTX 1060 is $0.022/hr. NEVER debug SSH plumbing on a $0.93/hr A100 — that mistake is most of the $0.22 above.

`vastai execute <id> "<cmd>"` is NOT a shell escape hatch (restricted whitelist, 400 Invalid command given).

EVERYTHING ELSE IN THE ROUTE IS PROVEN: offer search + ranking, create/destroy, and the cost ledger (reconciles with Vast invoices). SSH is the only blocker.
