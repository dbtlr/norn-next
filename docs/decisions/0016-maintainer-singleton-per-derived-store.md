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
keyed by the machine-local data base it lives under, the channel a build keeps its state under,
and the registered vault name, so two registrations differing in any of the three — a dev build
and a live build over one root, one root registered under two names, two people on one machine
with a network-mounted vault between them — key two stores, take two locks, and co-maintain.
Against each other they are ordinary concurrent writers of vault files, which is what a vault
has anyway: sync clients, editors, agents and humans write it concurrently, so correctness
against a concurrent writer is carried by fingerprint preconditions at the write and never by
exclusion.

What co-maintainership costs is duplicated work, and that cost is priced rather than prevented.
Two derived stores of one vault means the disk one store takes, taken twice, and — the expensive
half — every document's embedding and vector derivation computed twice, since a heal derives
against its own store and can read nothing from the other's. Every vault event is recorded once
in each store, off two recursive watcher registrations over one tree; those registrations are
drawn against a platform watch budget the whole machine shares, and exhausting it is not a bill
at all but watcher loss, which marks the entry that lost coverage untrusted until a demand
re-heals it. A home whose key nothing resolves any more — a vault deregistered or renamed, a
host moved to another data base — is residue nobody's keyed sweep opens again, and what bounds
it is the recursive age-thresholded sweep rather than anyone remembering to remove it. All of
that is a bill, not corruption. No norn lock ever restricts another process's access to vault
files, and one that tried would be honoured by none of the writers that matter.

**Every mechanism a maintainership owns is keyed by the same key the lock is, including the
shadow home when it falls back inside the vault.** A shadow home is normally the temporary
directory inside the derived directory, keyed by sitting there. Where the data directory is on
another filesystem the home falls back under the vault root, and a vault root is shared ground —
every host serving it can see it, under any key — so the fallback is keyed too: one home per
data base, channel and name, beneath the single dot-directory the walk excludes wholesale. That
is what makes the attach-time sweep sound. A freshly taken lock means no other host writes
through *this home*, so sweeping it with zero grace removes residue of writes that are already
over and can never reach a shadow another maintainer's write is still holding. The premise the
sweep rests on is the one the key makes true.

**What holds that, exactly.** The data-root placement is structural: the home *is* the
temporary directory inside the derived directory, so it is built from the same value the lock's
own path is, and the two cannot disagree. The fallback placement is carried by the key, which is
minted from that same value and carries all three of the lock's coordinates rather than a subset
of them — a key that dropped one would let two hosts take two locks and resolve one home, which
is the failure this decision exists to exclude. The join between them is neither: one named
function builds the key from the machine-local directories, and a test pins its parts against
the derived directory's own components. Where a type does not carry the invariant, a pin does,
and that is the whole of the chain.

This refines [0011](0011-shadows-live-outside-the-vault.md), which decided where a shadow lives
and recorded placement as decided *per vault at attach*: placement is per vault root **and** per
maintainership, and 0011's clause that leaked shadows in the fallback are swept is carried by
three tiers rather than one — the keyed home's own sweep, a zero-grace pass over the fallback
root where no home sits and a pre-keying build's residue does, and an age-thresholded recursive
pass that is the only thing that reaches a home whose key nothing resolves any more. 0011 stands
as accepted; this record is the correction.

The alternative was widening the lock to the vault root — one lock per root, so that two
registrations over one vault contend instead of co-maintaining. It loses on what it forbids and
on what it defends. It forbids the pair channel identity exists to allow: a dev build and a
released build over one vault is the arrangement the whole channel split is for, and a
root-scoped lock makes the second one refuse. It demands semantics nothing else here needs —
what a live host does when a dev host holds the root, how a takeover is asked for, when it is
granted — where the store-scoped lock needs none of that, because two stores were never in each
other's way. And it defends against no writer that matters: the vault is inherently multi-writer,
so a co-maintainer is one more fingerprint-checked writer beside the editors and sync clients
already there, and every one of them ignores a lock norn takes. What it would actually buy is
the duplicated work above, prevented by refusing to do the work at all.

The rest of the maintainer's terms stand as they were recorded. There is no on-disk mutation
lock: in-process serialization inside the host replaces the legacy one, which existed only
because the pre-host CLI wrote from many processes. Watchers only trigger reconciliation —
watcher loss or overflow marks the entry untrusted for re-healing rather than being trusted as
delivery. What 0012 scoped to the vault is wider than the lock's key and wider than exclusion
can carry: a vault-wide claim leaves the zero-grace sweep depending on no other host writing the
vault, which two co-maintainers falsify, and the keyed fallback home is what replaces that
dependence with a fact about one directory.
