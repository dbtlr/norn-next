---
status: accepted
date: 2026-08-09
---

# 0016 — the maintainer is singular per derived store, and every mechanism it holds is keyed like the lock

One flock admits at most one norn host as the maintainer of **one derived store**, and the lock
file sits inside the derived directory it protects. The co-location is what carries the
invariant: reaching the lock and reaching the store are the same act of path arithmetic, so no
second lock can stand in front of one store and no one lock can stand in front of two.
Exclusion carries exactly one thing — two maintainers of one derived store would corrupt it and
double every event it records — so becoming that store's maintainer is exclusive while
everything else stays open. This decision supersedes
[0012](0012-maintainer-singleton.md), which recorded the same exclusion for the same reason;
what it corrects is the scope of the claim and the placement rule that scope implies.

**The vault is not singular, and it is not singular even in maintainers.** A derived store is
keyed by the channel a build keeps its state under and the registered vault name, so two
registrations differing in either — a dev build and a live build over one root, or one root
registered under two names — key two stores, take two locks, and co-maintain. Against each
other they are ordinary concurrent writers of vault files, which is what a vault has anyway:
sync clients, editors, agents and humans write it concurrently, so correctness against a
concurrent writer is carried by fingerprint preconditions at the write and never by exclusion.
What co-maintainership costs is duplicated work, and that cost is priced rather than prevented —
two watchers over one tree, two heals, and every vault event recorded once in each store. It is
a bill, not corruption. No norn lock ever restricts another process's access to vault files,
and one that tried would be honoured by none of the writers that matter.

**Every mechanism a maintainership owns is keyed by the same key the lock is, including the
shadow home when it falls back inside the vault.** A shadow home is normally the temporary
directory inside the derived directory, keyed by sitting there. Where the data directory is on
another filesystem the home falls back under the vault root, and a vault root is shared ground —
every host serving it can see it, under any key — so the fallback is keyed too: one home per
channel-and-name, beneath the single dot-directory the walk excludes wholesale. That is what
makes the attach-time sweep sound by construction rather than by assumption. A freshly taken
lock means no other host writes through *this home*, so sweeping it with zero grace removes
residue of writes that are already over and can never reach a shadow another maintainer's write
is still holding. The premise the sweep rests on is the one the key makes true.

The rest of the maintainer's terms stand as they were recorded. There is no on-disk mutation
lock: in-process serialization inside the host replaces the legacy one, which existed only
because the pre-host CLI wrote from many processes. Watchers only trigger reconciliation —
watcher loss or overflow marks the entry untrusted for re-healing rather than being trusted as
delivery. What 0012 scoped to the vault is wider than the lock's key and wider than exclusion
can carry: a vault-wide claim leaves the zero-grace sweep depending on no other host writing the
vault, which two co-maintainers falsify, and the keyed fallback home is what replaces that
dependence with a fact about one directory.
