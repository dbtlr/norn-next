---
status: accepted
date: 2026-08-03
---

# 0013 — quarantine is per document, refusal is per vault

A document norn cannot decode is quarantined rather than refused: the rung-2 heal skips its facts, records a finding naming the path and the cause class (withheld while a readable document stands at the finding's rendered spelling), and keeps going, so the entry reaches `Ready` and serves every other document. There are three cause classes — path bytes that are not UTF-8, a path spelling the document-path grammar refuses, and a body that is not UTF-8 — and every rung-2 path answers them the same way, because a document the vault holds and norn cannot read is a fact about that document rather than about the vault. The store holds only representable truth, so a document that stops decoding loses its store row; the row's death is recorded as a quarantine, which is a death of the derived row and not of the file, and reading it as a prune or a removal would say the document left the vault. Recovery needs no separate mechanism: a document that reads again is an ordinary derivation, and the increment's own findings discard takes the finding with it.

**Environmental failures are never quarantined, and that prohibition is what makes the rest safe.** A schema that will not read, a store that will not open, a walk that cannot list a directory, a path whose permissions were revoked — each of these refuses and leaves the entry untrusted, because the environment is broken and the stored state is not. Quarantine turns "this cannot be read" into "this is absent, and here is why", which is correct exactly when the vault really does hold something unreadable and catastrophic when the vault is fine and the environment is not: widening it to an unreadable directory would prune every document under it and call the pruning derived truth. So the boundary is the class of causes, not the shape of the failure, and a cause that is not one of the three named above is a refusal.
