---
status: accepted
date: 2026-07-31
---

# 0012 — the maintainer is singular, the vault is not

One flock per vault admits at most one norn host as the maintainer of its derived state; there is no on-disk mutation lock, and no norn lock ever restricts another process's access to vault files. A vault is inherently multi-writer — sync clients, editors, agents, and humans write it concurrently — so correctness against concurrent writers is carried by fingerprint preconditions and watcher-observed drift, never by exclusion. Exclusion carries exactly one thing: two maintainers would corrupt one derived store and double every event, so becoming the maintainer is exclusive while everything else stays open. In-process serialization inside the host replaces the legacy mutation lock, which existed only because the pre-host CLI wrote from many processes.
