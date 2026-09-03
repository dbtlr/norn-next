//! The hold an entry keeps on itself.
//!
//! The epoch, the scheduling gate, the leg running against the entry, the queue
//! slot and the coverage the entry's work runs against are one another's
//! invariants, and the fields carrying them are private to this module: every
//! move any of them makes is a method here, so a caller can neither write a
//! state no move produces nor read one out of the shape it is stored in.
//!
//! # What each piece carries
//!
//! Every invariant below names the field or move carrying it and the test
//! pinning it. A pin is named only where removing the move it names fails that
//! test. A piece carrying no invariant, and an invariant no test pins, are each
//! named as such: what the machinery does not carry, and what nothing holds it
//! to, are as much of the map as what it does.
//!
//! Two things are called a gate. `Entry::gate` is the mutex over an entry's
//! whole lifecycle state — the entry gate lock every move below is taken under,
//! and the one the architecture documents name. The entry's served name and
//! root stand outside it, unchanging for as long as the serving set serves the
//! entry, so nothing below reads or writes them. `Claim::gate` is the scheduling [`Gate`]
//! inside the state, which is what this section means by "the gate" throughout.
//! One is nested in the other.
//!
//! ## The gate
//!
//! **An entry whose gate is taken takes no second claim.** Carried by [`Gate`]
//! and read through [`Claim::is_held`], which every producer of work tests
//! before it schedules: [`Claim::begin_poll`] and [`Claim::begin_job_leg`]
//! write [`Gate::Held`], and a dispatcher tick passes over a held entry. Pinned
//! by `a_tick_cannot_claim_an_entry_whose_next_job_is_already_in_the_queue` and
//! `concurrent_demand_is_single_flight`.
//!
//! **A marker holds the gate in its own right, so a claim gives the gate back
//! only where nothing is scheduled.** Two moves carry this and each has its own
//! pin. The guard in [`Claim::release`] is what leaves the marker standing when
//! a claim ends over it, pinned by
//! `a_job_that_loses_the_attachment_to_a_poll_runs_when_the_poll_gives_it_back`.
//! [`Claim::is_held`] reading [`Gate::Scheduled`] as taken is what keeps a
//! producer from claiming the entry the marker holds, pinned by
//! `a_tick_passes_over_an_entry_a_marker_holds`. Without either, a claimant
//! takes an entry out from under work already scheduled against it, and the job
//! that marker stands for is one no dispatch reaches.
//!
//! **Work nothing holds the entry for is work no dispatch reaches.** Carried by
//! [`Claim::drop_marker`] and [`Claim::open`]: a claim losing the coverage its
//! scheduled job was to run against gives the marker back with the gate. Pinned
//! by `a_claim_that_loses_coverage_drops_the_job_that_was_waiting_on_it`,
//! `a_claim_that_refuses_environmentally_drops_the_job_that_was_waiting_on_it`
//! and `a_release_takes_the_marker_of_the_job_that_was_waiting_on_the_claim`.
//!
//! **A marker a claim did not plant outlives the claim standing over it.** Two
//! moves carry this. [`Claim::restore`] yields to a job already scheduled
//! because that job is the newer of the two, pinned by
//! `a_job_that_lost_its_coverage_leaves_a_newer_marker_standing`.
//! [`Claim::mark`] is how a refused send takes the marker back for the work the
//! entry owes now, pinned by
//! `a_refused_follow_up_takes_the_marker_from_work_the_entry_has_left` and
//! `failed_send_releases_the_marker_after_a_newer_epoch_is_installed`.
//!
//! **A gate held across a hand-on is an entry nothing takes between the work it
//! moves on from and the work it moves on to.** Carried by [`Claim::hold`],
//! reached from [`Claim::hand_on`] alone, and by [`Claim::hand_off`], which
//! leaves the gate to the job going into the channel. Pinned by
//! `a_tick_cannot_claim_an_entry_whose_next_job_is_already_in_the_queue`.
//!
//! **A marker dropped under a running leg leaves the gate to that leg.**
//! Carried by [`Claim::drop_marker`]'s read of [`Claim::leg`]. The derived gate
//! is what decides the outcome at three sites, where no caller opens the gate
//! afterwards under the same lock: `refuse_identity_error`'s arm for coverage
//! out with a leg, which opens the gate only where none is registered;
//! `run_job`'s superseded-arrival arm, which drops the marker and returns; and
//! `refuse_conflict`'s in-flight path, where the continue skips the open below
//! it. Pinned by
//! `a_stale_arrival_gives_its_marker_back_to_the_leg_running_against_the_entry`.
//!
//! ## The epoch
//!
//! **A leg answers for itself alone once the entry has moved past its epoch.**
//! Carried by `epoch`, [`Claim::stands_at`] and [`Claim::supersede`]: every leg
//! carries the epoch it was taken at and rechecks it under the entry's lock
//! before it writes anything back. Pinned by
//! `a_superseded_poll_gives_the_entry_back_and_schedules_the_lease_on_it`,
//! `an_arrival_at_a_superseded_epoch_leaves_the_slot_of_the_job_that_replaced_it`
//! and `an_identity_refusal_invalidates_an_in_flight_reconcile`.
//!
//! **Work an invalidation supersedes goes with the epoch it was raised under.**
//! Carried by [`Claim::invalidate`], where the supersession, the abandoned slot
//! and the dropped marker are one move: an entry left holding any one of them
//! is one a later dispatch reaches for work it has moved on from. All three
//! limbs are pinned. The supersession is pinned through
//! [`Claim::stands_at`] by `an_identity_refusal_invalidates_an_in_flight_reconcile`
//! and `a_refused_entry_is_polled_by_nothing_afterwards`, and the abandoned
//! slot by `an_invalidation_stops_the_entry_waiting_on_the_job_it_supersedes`.
//! The dropped marker is pinned by
//! `an_identity_refusal_drops_work_scheduled_against_the_poll_it_invalidates`:
//! without it, the stale marker outlives the poll and holds the gate against
//! the re-attach demanded after the refusal.
//!
//! ## The leg
//!
//! **Coverage out with a leg comes back where that leg ends.** Carried by the
//! `leg` registration, which the teardowns read to decide whether to publish a
//! release themselves or open a window the leg's own end closes. Pinned by
//! `a_refusal_over_a_leg_holding_none_of_the_coverage_closes_its_window_at_the_leg`,
//! `destruction_leaves_coverage_out_with_a_leg_to_that_leg` and
//! `a_refused_alias_held_in_a_poll_releases_through_the_poll_that_holds_it`.
//!
//! **A leg holding none of the entry's coverage is not what ends it.** Carried
//! by [`Claim::end_running_leg`], which the refusals that take coverage out of
//! the entry's own hand pair with the taking. Pinned by
//! `a_refusal_over_a_leg_standing_at_parked_coverage_releases_it_inline` and
//! `an_identity_refusal_over_a_leg_standing_at_parked_coverage_releases_it`.
//!
//! **One release ends whichever leg reached it.** Carried by
//! [`Claim::end_leg`], epoch-typed and deliberately blind to the kind, because
//! a poll claim and a job leg come to that one completion alike. Pinned by
//! `a_refusal_over_a_leg_holding_none_of_the_coverage_closes_its_window_at_the_leg`,
//! which is the case that fails where this move ends nothing.
//!
//! **A release a demand outlived re-arms under the entry's own claim.** Carried
//! by the [`Claim::hand_on`] and [`Claim::take_slot`] pair ending the release,
//! taken under the lock publishing the state it warms into. The hand-on is
//! pinned by `a_job_leg_release_honors_a_demand_with_a_claimed_re_attach` and
//! `a_demand_raised_during_a_teardown_is_honored_when_the_release_finishes`.
//! The re-arm is the tail every teardown takes and the one the cleanup after an
//! unwind does not — a decision `finish_release` is told rather than derives —
//! so this pair is reached over work an entry chose to end, never over work
//! that ended part way through. Pinned there by
//! `a_lease_held_across_an_unwind_buys_one_attempt_and_the_next_demand_asks_again`.
//! The slot the pair takes is what holds the entry to the attach the re-arm
//! sends: the re-arm plants no marker of its own, so the reader that refuses a
//! second send — [`Claim::take_slot_for_marked`] — has the slot alone to read a
//! marker recorded beside it against.
//!
//! That reader is a dormant carrier over this slot. No producer the current
//! call graph reaches records a marker beside a standing slot: the ones that
//! schedule work read an unheld gate first, the one that marks a refused send
//! frees the slot under the same lock, and the hand-on above holds the gate
//! for the whole window this slot stands — so today the slot and its absence
//! part in nothing a run observes. Layer 4's request-driven work submission is
//! the producer that parts them, naming the work an entry owes at the epoch it
//! stands at whether the gate is held or not. The slot is kept for it and
//! covered at its seam by
//! `a_release_re_arm_holds_the_queue_slot_against_a_second_send`, which writes
//! that producer's marker itself, drives a tick against a queue with room for
//! a second send, and reads the room the tick leaves.
//!
//! **A poll and a job leg do not end each other.** Carried by the [`Leg`] kind
//! and the equality checks in [`Claim::end_poll`] and [`Claim::end_job_leg`].
//! The state where the two stand at one epoch is reached on every run: a poll
//! takes the entry and blocks holding its coverage, a job dispatched at that
//! same epoch reaches a worker, and `run_job` registers a job leg over the
//! poll's registration before the poll has returned. A poll returning in that
//! window ends against a job leg's registration, and the kind check is what
//! makes it a no-op. An epoch-typed end would clear the running leg's
//! registration instead, and a release window opened over that leg would then
//! have nothing left to close it. Pinned by
//! `a_poll_end_leaves_the_job_leg_a_release_window_waits_on`.
//!
//! The guard in [`Claim::end_poll`] is partial by construction: the kind
//! decides the registration alone, and the [`Claim::release`] beside it runs
//! whatever leg is registered. A poll ending over another leg therefore gives
//! the gate back, and what holds the entry from there is the marker or the
//! leg's own later moves rather than this call.
//!
//! **The leg outlives the gate it took.** Carried by the `leg` registration
//! standing after [`Claim::release`] and read by
//! [`Claim::take_slot_for_marked`], so a dispatcher tick never sends the job a
//! running leg is about to send itself. Pinned by
//! `a_tick_takes_no_slot_for_a_marker_the_running_leg_sends_itself`.
//!
//! ## The queue slot
//!
//! **The slot names its occupant, so only that job's own arrival frees it.**
//! Carried by [`Slot`]'s epoch and the equality in [`Slot::free`]: a slot freed
//! under the job that replaced it is that job sent into the channel a second
//! time. Pinned by
//! `an_arrival_at_a_superseded_epoch_leaves_the_slot_of_the_job_that_replaced_it`.
//!
//! **The job in the channel and the slot naming it are installed together, and
//! given back together.** Carried by [`Claim::take_slot_for_marked`], which
//! takes the slot under the lock that reads the marker, and by
//! [`Claim::free_slot`] where a full queue refuses the send. Pinned by
//! `worker_defers_followup_when_a_sibling_fills_its_only_queue_slot`,
//! `a_refused_follow_up_takes_the_marker_from_work_the_entry_has_left` and
//! `failed_send_releases_the_marker_after_a_newer_epoch_is_installed`. The slot
//! [`Claim::hand_off`] takes under the lock ending the claim is what stands for
//! the job where the hand-off leaves no marker naming it, and is pinned by
//! `an_entry_holding_its_queue_slot_stays_in_the_set`, which drives the entry
//! through a hand-off over an open gate and reads the slot it left.
//!
//! **A taken slot is a hold on the entry in its own right.** Carried by
//! [`Claim::slot_taken`], which `EntryState::held_by_anything` reads beside the
//! gate and the leg registration: a hand-off can take the slot with the gate
//! already open, so a job entering the channel is held by the slot alone.
//! Pinned by `an_entry_holding_its_queue_slot_stays_in_the_set`.
//!
//! ## The coverage
//!
//! **Coverage has one holder, and custody is read rather than inferred.**
//! Carried by [`Custody`]: [`Coverage::take`] takes from [`Custody::Parked`]
//! alone and [`Coverage::give_up`] gives up that alone, each leaving the record
//! of a leg's hold standing, so coverage already out with a leg is taken by no
//! other and given up by none. The two limbs stand apart.
//! [`Coverage::give_up`] leaving the record is pinned by
//! `destruction_leaves_coverage_out_with_a_leg_to_that_leg`, which is the case
//! that fails where that record is discarded and a teardown then reads an entry
//! holding nothing as an entry with nothing out. [`Coverage::take`] leaving it
//! is pinned by `a_refused_take_leaves_the_record_of_the_leg_holding_the_coverage`:
//! a non-`Parked` take recording [`Custody::None`] instead leaves an entry
//! accounting for nothing while a leg still holds its coverage, and the pass
//! `Host::drop` runs after the joins reads that record with no registration to
//! fall back on. Both release-window arms pair the record with the leg
//! registration under an `||`, so that bare read is where the two spellings
//! part.
//!
//! [`Coverage::out_with_leg`] reading that record rather than inferring it from
//! an empty attachment has four readers. The debug assertion in
//! `refuse_conflict` states the invariant outright, and
//! `a_lease_stops_answering_a_conflict_a_later_recheck_retired` is the case
//! that reaches it. The two release-window arms — `refuse_conflict`'s and the
//! one in `Host::drop` — read it beside the leg registration under an `||`, so
//! an entry accounting for no coverage and one whose coverage is out with a leg
//! take the same route there. `Host::drop`'s post-join pass reads it bare, with
//! no registration to fall back on, and continues over what it finds still out:
//! inferred custody puts an entry accounting for no coverage on that continue,
//! and `TrustState::Unattached` becomes a state destruction never publishes.
//! Two cases pin that on the production path, debug assertions on or off:
//! `destruction_releases_before_it_publishes_unattached`, which asserts the
//! state the destruction publishes, and
//! `a_lease_stops_answering_a_conflict_a_later_recheck_retired`, whose
//! `wait_for_state` times out.
//!
//! **A leg ends the coverage it took alone.** Carried by [`Custody::OnLeg`]'s
//! epoch and the equality in [`Coverage::released_by`]: coverage a leg parked
//! back, or that another leg has since taken, is not that leg's to end. Pinned
//! by
//! `a_refusal_over_a_leg_holding_none_of_the_coverage_closes_its_window_at_the_leg`
//! and `destruction_gives_back_an_attachment_a_finished_job_left_behind`.
//!
//! **Custody says who holds the coverage, and nothing about who holds a pin.**
//! The two part at three places: an attach leg holds an attachment it never
//! took from the entry and pins nothing,
//! `Job::Detach` takes the coverage and pins nothing, and every leg past its
//! epilogue is still recorded as holding what it is handing to
//! [`EntryOps::detach`](super::EntryOps::detach). No move here is read as a pin
//! for that reason — the entry names its pinning leg itself, at
//! `EntryState::pinned_leg`, and the row for that field is under *What this
//! module is not*.
//!
//! ## The closing path
//!
//! Every teardown enters at `begin_release`, which opens the gate
//! unconditionally and raises the flag saying the entry's resources are going
//! back, and every teardown ends at `finish_release`, which holds the only call
//! site of [`Claim::end_leg`]. The lifetime rule lives in that seam rather than
//! at the sites: a caller decides whether to release, and the seam decides what
//! a release does to the claim. `finish_release`'s own [`Claim::open`] is the
//! terminal revocation on that path — the window's start opened the gate
//! already, and this is the one that stands. What routes around the seam is
//! `refuse_identity_error`, which gives the entry's coverage to the ops without
//! opening a window: that is a park with the resources handed back under it,
//! and the entry stays untrusted and owing a recovery rather than released.
//!
//! One lifetime beside the claim's rides the same seam. The entry's reader goes
//! back at `begin_release` rather than where the resources reach the ops,
//! because the window's start is where the entry stops being readable and the
//! store the handle was minted from closes inside the window; what routes
//! around the seam lets go of the handle itself. The rows for it are under
//! *What this module is not*, with the rest of the entry state.
//!
//! Two moves revoke a claim blind to the kind and the epoch it stands at:
//! [`Claim::end_running_leg`], which ends whatever is registered, and the
//! [`Claim::open`] beside it in `refuse_conflict`, `refuse_identity_error`,
//! `Host::drop` and the demand that takes back a scheduled teardown.
//! `reclaim_unwound_leg` reaches the second of them through `begin_release`,
//! over a leg that will never take another lock. Every one of those sites is
//! preceded by [`Claim::invalidate`] under the same lock, and that
//! supersede-first order is what makes the blindness safe: the entry has moved
//! past every epoch a leg could be standing at before anything is revoked, so
//! what these end can no longer write anything back.
//!
//! The remaining calls to [`Claim::open`] revoke nothing another claim holds.
//! `begin_release` and `finish_release` open the gate on the entry's own
//! teardown path, and `restore_lost_claim` opens it over the very job it is
//! ending, at that job's own epoch.
//!
//! ## What carries nothing
//!
//! `Claim::slot`, `Slot::job` and `Claim::stand_at` carry no invariant: they
//! are two test readers and a test writer, and nothing the entry does turns on
//! them. The gate names no holder of its own, because the leg registration is
//! what says which claim holds it, and a holder beside it is one fact spelled
//! twice.
//!
//! # What this module is not
//!
//! Three facts about an entry sit next to the claim and are carried outside it.
//! A carve that moves the claim expects to find them here and does not.
//!
//! **The pin discipline.** `EntryState::safety_pins`, with `pin`, `unpin` and
//! `pinned` beside it, lives in the entry state rather than in the claim. It
//! carries a narrower fact than any field here: that work running outside the
//! entry's lock comes back to a lock of its own, so what it holds is coming
//! back. Three readers turn on it, and they read it in opposite directions.
//! `schedule_due_detach` refuses to schedule a teardown while a pin stands, so
//! nothing tears an entry down under a leg that is coming back to it.
//! `reap_idle_shared` marks an idle entry due where a pin stands even though its
//! coverage is out, because the leg holding that coverage is what brings it
//! back. `restore_lost_claim` decides between [`Claim::open`] and
//! [`Claim::restore`] on it: a job that lost its coverage records itself for a
//! later tick where a pin says the coverage is coming back, and ends there where
//! none does. Pinned by
//! `a_job_that_loses_the_attachment_to_a_poll_runs_when_the_poll_gives_it_back`.
//!
//! One writer gives a pin back for a leg rather than as one. A leg that unwinds
//! reaches no lock of its own, so `reclaim_unwound_leg` gives the pin back on
//! its behalf, and `EntryState::pinned_leg` is what says such a pin is
//! standing: every pinning leg takes the coverage and the pin under one hold of
//! the entry's lock, records itself there as it takes them, and clears the
//! record at the lock that ends it. A pin is therefore given back for a leg
//! exactly where the leg is the one the entry is pinned for — never on a
//! reading of what the leg was holding, which stands over legs that pin nothing
//! and over legs already past their own end. Pinned by
//! `a_pinning_leg_that_unwinds_gives_its_own_pin_back_and_no_other`, which is
//! the case that fails where it stops reaching the leg that took one.
//!
//! The counterpart case — a non-pinning leg's unwind reaching this while a
//! read's pin stands beside it — has no leg left to carry it. `EntryOps::detach`
//! was the one call a non-pinning leg made outside its own lock, and `give_back`
//! catches its panic before this is ever reached, so a detach panic completes
//! the release it was part of rather than unwinding into a reading `unpin_leg`
//! could widen past the record. `a_detach_panic_on_an_idle_leg_completes_the_release_with_a_reads_pin_standing`
//! pins that a read's pin survives such a panic through the give-back instead.
//!
//! A read running against the entry takes one too, which is what makes it work
//! the discipline covers rather than work beside it: `Host::begin_read` takes
//! the pin under the lock it reads the entry's handle in, and `ReadHold`'s drop
//! gives it back. Pinned by
//! `a_read_in_flight_holds_the_entry_against_an_idle_teardown`.
//!
//! **The reader slot.** `EntryState::reader`, with `install_coverage`,
//! `close_reader` and `Host::begin_read` beside it, lives in the entry state
//! for the reason the pin does: it is a fact about the coverage the entry is
//! served from, and no move here reads or writes one.
//!
//! *A reader is minted where the coverage it reads is installed.* Carried by
//! `EntryState::install_coverage`, which mints the handle and installs the
//! coverage as one move, under the lock that publishes the trust label beside
//! them: an attach that installs nothing mints nothing, and a handle minted
//! under a later lock is one minted from coverage the entry may have given back
//! already. The fusion of the two moves is carried by construction rather than
//! by a test: [`Coverage`] hands out no borrow of what it holds, so no mint
//! beside the install can read the attachment in place, and a test that
//! watched the two land together would pass unchanged over a mint moved to a
//! lock of its own. What the tests pin is the rest of the row — one mint per
//! install, published where the entry publishes its trust label and readable
//! there, by
//! `an_attach_publishes_a_reader_beside_the_coverage_it_installs`; and no mint
//! where nothing is installed, by
//! `an_attach_the_entry_moved_on_from_mints_no_reader` and
//! `an_attach_that_installs_no_coverage_mints_no_reader`. Coverage that mints
//! no handle of its own leaves the slot empty and the entry answering no read,
//! which is the production configuration today and is pinned by
//! `coverage_that_mints_no_reader_leaves_an_entry_no_read_reaches`.
//!
//! *A reader goes back before the store it was minted from closes.* Carried by
//! `begin_release`, which lets go of the handle at the window's start, so the
//! one site every teardown enters carries the rule for all of them; and by
//! `refuse_identity_error`, which routes around that seam and lets go of the
//! handle itself, on both the routes its coverage takes to the ops. Pinned by
//! `a_teardown_closes_the_reader_before_the_store_goes_back`,
//! `a_refusal_over_a_leg_holding_the_coverage_closes_the_reader_at_the_window`
//! `an_identity_refusal_closes_the_reader_it_gives_the_coverage_back_with` and
//! `an_identity_refusal_over_a_leg_holding_the_coverage_closes_the_reader`.
//!
//! `finish_release` lets go of nothing, and no mint lands inside the window it
//! ends. Every path to it passed through `begin_release`, which closed the
//! reader. The one mint site is the attach publication arm, which publishes
//! under `Claim::stands_at` at its own epoch, and a window is opened in one of
//! two ways, neither of which leaves such an attach standing. A move that
//! supersedes first — `refuse_conflict`, `Host::drop`, the `poll_watchers`
//! failure arms — has taken the entry past every epoch an attach could be
//! standing at before it opens one. A move that supersedes nothing — the
//! Recover, Rebuild, Reconcile and Maintenance failure arms, and `Job::Detach` — is the
//! leg standing at the entry's own current epoch, and one epoch admits one leg,
//! so there is no attach there to publish. The `debug_assert!` at the top of
//! `finish_release` is where that argument stops being prose.
//!
//! Two sites give coverage to `EntryOps::detach` through neither
//! `begin_release` nor the route around it: the stale arm of `poll_watchers`
//! with no window standing, and the non-releasing arm of `end_job_leg`. Neither
//! closes a reader, and neither has to. The coverage they carry was out with a
//! leg, and every move that can take an entry past a leg's epoch while that leg
//! holds coverage closes the reader before the leg comes back:
//! `refuse_conflict`, `refuse_identity_error` and `Host::drop` close it where
//! they move, and the `poll_watchers` failure arms supersede their own leg's
//! epoch inside `begin_release`, which closed it already. The one epoch-mover
//! that closes nothing, `Host::demand`'s invalidate, moves only over a
//! scheduled detach — a flag set with the coverage in hand and no claim held,
//! so no leg is out with the coverage it moves past. The `debug_assert!` at
//! each site is what an epoch-mover added without that close runs into.
//!
//! *A read holds the entry's own handle, and holds it beside the entry.*
//! Carried by `Host::begin_read`, which clones the handle, reads the trust
//! label and takes the pin under one hold of the entry gate lock — the coupling
//! the trust row below is the other half of — and by `ReadHold`'s drop, which
//! gives the pin back. The handle is shared rather than taken out, so a read in
//! flight leaves the slot where the next read finds it, and the teardown that
//! empties that slot closes no handle a read is running on. The sharing is
//! pinned by `concurrent_reads_share_the_entrys_one_reader`. The outliving is
//! characterized by `a_reader_a_read_is_running_on_outlives_the_entrys_own`
//! rather than pinned by it: what that case asserts is what an `Arc` clone
//! does, so it reddens on a redesign of the slot's type and on no mutation of
//! the moves around it.
//!
//! What a read's pin buys is what a leg's pin buys and no more — the three
//! readers named above are the whole of it — and under a read it buys strictly
//! less than under a leg. A leg holds the entry's coverage for as long as its
//! pin stands, so `refuse_conflict` takes nothing and leaves the give-back to
//! the leg's own end, and `Host::drop` joins the workers before it gives
//! anything back. A read holds no coverage: at both those sites the entry is
//! still holding its own, so the take answers with it and the refusal or the
//! destruction is itself what reaches `EntryOps::detach` — under the live hold,
//! rather than at a leg's end.
//!
//! Three shapes of teardown reach an entry with no pin read at all, and a read
//! in flight stops none of them. A refusal or a destruction moves first and
//! consults no pin. An idle detach already scheduled is the second:
//! `schedule_due_detach` reads the pin where it schedules and nowhere after, so
//! a `Job::Detach` scheduled before `Host::begin_read` takes its hold tears the
//! entry down under that hold — the reap-then-read order, characterized by
//! `a_detach_scheduled_before_a_read_tears_the_entry_down_under_it`. The third
//! is a job leg failing its way into a release: the `LostMaintainership` and
//! `MaintainerContended` arms of `poll_watchers` and their Recover, Reconcile
//! and Maintenance equivalents run `begin_release` and hand the coverage on
//! without reading a pin either.
//!
//! That a read in flight keeps reading through any of them is carried by the
//! handle it holds and by nothing else — no move states that the file behind
//! that handle outlives the teardown, and nothing pins one that would.
//!
//! **The trust label and the instant it is a snapshot of.** `EntryState::trust`,
//! the phases written around it, and the rule that a label is published under
//! the same lock as the move it names all live in the entry state. The claim
//! neither reads nor writes a trust state, and no invariant here constrains one:
//! what couples a label to the instant it describes is the lock the two are
//! written under, and that coupling is the entry's, not the claim's.

use super::Job;

/// An entry's coverage, and who holds it.
///
/// Coverage is the attachment a vault is served from: the watcher, the store
/// and the maintainer lock. One holder has it at a time — the entry itself, or
/// a leg running against the entry — and this says which, so custody is read
/// rather than inferred from an empty attachment beside a registered leg.
///
/// A leg is registered against the entry from the instant it begins and holds
/// the coverage only from the instant it takes it, so a leg standing over
/// parked coverage is a state of its own rather than a contradiction.
///
/// The custody below is reached through the moves alone, and every move names
/// the leg making it: a leg takes what the entry holds and none other, parks
/// back what it took, and ends the coverage it took alone.
pub(super) struct Coverage<A>(Custody<A>);

/// Who holds an entry's coverage.
enum Custody<A> {
    /// The entry has none.
    None,
    /// The entry holds its own coverage, and any work that runs against the
    /// entry takes it from here.
    Parked(A),
    /// The leg at this epoch holds the entry's coverage. What becomes of it —
    /// parked back, handed to a release, or given to
    /// [`EntryOps::detach`](super::EntryOps::detach) — is that leg's to say
    /// until it ends.
    ///
    /// A leg here is whatever the entry handed its coverage to at that epoch:
    /// the claim registered in [`Claim::leg`], or a release the entry began
    /// itself, which registers no claim. One of them holds it at a time —
    /// [`Coverage::take`] takes from [`Custody::Parked`] alone, so coverage
    /// already out with a leg is taken by no other, whatever epoch that other
    /// stands at.
    OnLeg(u64),
}

impl<A> Coverage<A> {
    /// An entry accounting for no coverage: none in its own hand, and none out
    /// with a leg.
    pub(super) fn none() -> Self {
        Self(Custody::None)
    }

    /// Whether the entry itself holds its coverage. Work runs against coverage
    /// in the entry's own hand alone: coverage a leg holds is that leg's until
    /// it ends, and an entry with none is one an attach is what serves.
    pub(super) fn in_hand(&self) -> bool {
        matches!(self.0, Custody::Parked(_))
    }

    /// Whether the entry's coverage is out with a leg. What a leg holds is
    /// that leg's until it ends, so a teardown reaching an entry here has
    /// nothing of its own to give back: what the leg holds comes back where
    /// the leg ends.
    pub(super) fn out_with_leg(&self) -> bool {
        matches!(self.0, Custody::OnLeg(_))
    }

    /// Take the coverage the entry holds, for the leg at this epoch. An entry
    /// holding none gives none, and coverage already out with a leg is taken by
    /// no other.
    pub(super) fn take(&mut self, leg: u64) -> Option<A> {
        match std::mem::replace(&mut self.0, Custody::OnLeg(leg)) {
            Custody::Parked(coverage) => Some(coverage),
            held => {
                self.0 = held;
                None
            }
        }
    }

    /// Take the coverage the entry holds and give it up: the caller returns it
    /// to [`EntryOps::detach`](super::EntryOps::detach), and the entry accounts
    /// for none from here.
    ///
    /// Coverage out with a leg is not the caller's to give up, and the record
    /// of the leg holding it stands: that leg is what ends it.
    pub(super) fn give_up(&mut self) -> Option<A> {
        match std::mem::replace(&mut self.0, Custody::None) {
            Custody::Parked(coverage) => Some(coverage),
            held => {
                self.0 = held;
                None
            }
        }
    }

    /// Park the coverage this leg took back in the entry, which holds it from
    /// here.
    ///
    /// A leg parks back what it took: coverage another leg holds is that leg's
    /// until it ends, and parking over it would leave the entry holding
    /// coverage with a leg still out with the entry's own.
    pub(super) fn park_by(&mut self, leg: u64, coverage: A) {
        debug_assert!(
            matches!(self.0, Custody::OnLeg(held) if held == leg)
                || matches!(self.0, Custody::None),
            "coverage parked by a leg that is not what holds it"
        );
        self.0 = Custody::Parked(coverage);
    }

    /// Put coverage an attach installed into an entry that holds none. What an
    /// attach installs is coverage the entry never had, so this takes nothing
    /// out of any leg's hands.
    pub(super) fn install(&mut self, coverage: A) {
        debug_assert!(
            matches!(self.0, Custody::None),
            "coverage installed over coverage the entry accounts for"
        );
        self.0 = Custody::Parked(coverage);
    }

    /// The coverage this leg took has gone where the leg sent it, so the entry
    /// holds none. Coverage the leg parked back, or that another leg has since
    /// taken, is untouched: what a leg gave back is not the leg's to end.
    pub(super) fn released_by(&mut self, leg: u64) {
        if matches!(self.0, Custody::OnLeg(held) if held == leg) {
            self.0 = Custody::None;
        }
    }
}

/// What holds an entry's scheduling gate.
///
/// The gate is what every producer of work tests before it schedules against an
/// entry: an entry whose gate is taken already owes the work whatever holds it
/// is doing. A job scheduled against the entry holds the gate itself, so a
/// marker can never stand over an open gate — that is a job no dispatcher tick
/// would ever reach.
enum Gate {
    /// Nothing holds the entry: a claim may take it, and work may be scheduled
    /// against it.
    Open,
    /// A claim holds the entry with no job standing behind it.
    ///
    /// Which claim that is, is read from [`Claim::leg`] rather than carried
    /// here: every move that leaves the gate held writes the leg registration
    /// under the same lock, so a holder named here would be a second spelling of
    /// that one field.
    ///
    /// A gate held with no leg registered is a job the entry has handed to the
    /// job channel and no leg has begun — for as long as any reader can see it.
    /// A teardown that ends the registration before it takes the gate back
    /// stands in that shape between the two writes, with nothing in the channel
    /// either; both writes are under one hold of the entry gate lock, so the
    /// shape never outlives it.
    Held,
    /// The next job the entry owes, waiting on a dispatcher tick to send it.
    Scheduled(Job),
}

/// A claim running against an entry.
///
/// A watcher poll and a job leg both take the entry and both give it back, and
/// they end at different sites: a poll ends where its own tick ends it, a job
/// leg where the epilogue that dispatched it does. The kind is carried here so
/// neither ends the other, at the epoch they may both stand at.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Leg {
    /// A watcher poll, at the epoch the entry stood at when it took it.
    Poll(u64),
    /// A job leg, at the epoch its job carries.
    Job(u64),
}

impl Leg {
    /// The epoch the leg was taken at.
    pub(super) fn epoch(self) -> u64 {
        match self {
            Self::Poll(epoch) | Self::Job(epoch) => epoch,
        }
    }
}

/// The one job an entry is waiting on in the job channel.
///
/// The slot names its occupant, so only that job's own arrival gives it back:
/// an arrival at a superseded epoch answers for itself alone, and a slot freed
/// under the job that replaced it is that job sent into the channel a second
/// time. Every move the slot makes names the job making it, so a job can neither
/// free a slot it does not hold nor read one it does.
#[derive(Default)]
struct Slot(Option<u64>);

impl Slot {
    /// Whether a job is waiting in the slot.
    fn taken(&self) -> bool {
        self.0.is_some()
    }

    /// The job waiting in the slot, where one is waiting in it. Nothing the
    /// entry does turns on which job that is — every move the slot makes names
    /// the job making it — so this reads the slot for tests alone.
    #[cfg(test)]
    fn job(&self) -> Option<u64> {
        self.0
    }

    /// Whether this job is the one waiting in the slot.
    fn holds(&self, job: u64) -> bool {
        self.0 == Some(job)
    }

    /// Take the slot for this job, out from under whatever holds it. The caller
    /// sends this job under the same lock, so a slot it takes over names work
    /// the entry has moved on from: that job's arrival answers for itself alone,
    /// and the slot it no longer holds is one it cannot free.
    fn take(&mut self, job: u64) {
        self.0 = Some(job);
    }

    /// Give back the slot a send took, where this job is what holds it. A slot
    /// standing for a job no channel holds is an entry every later dispatch
    /// refuses; a slot standing for another job is that job's own occupancy, and
    /// taking it away is what sends that job twice.
    fn free(&mut self, job: u64) {
        if self.holds(job) {
            self.0 = None;
        }
    }

    /// Stop waiting on the job holding the slot. What that job's arrival finds
    /// is an entry that has moved past it, and the work that replaced it is not
    /// held behind a job with nothing left to do.
    fn abandon(&mut self) {
        self.0 = None;
    }
}

/// An entry's claim on itself.
///
/// The epoch is what every leg carries and rechecks: an entry that has moved on
/// from a leg's epoch has superseded the work that leg was scheduled for, and
/// the leg answers for itself alone from there. The gate, the running leg and
/// the queue slot are all named by epochs raised under it.
pub(super) struct Claim {
    epoch: u64,
    gate: Gate,
    /// The leg running against the entry, where one is running.
    ///
    /// The leg outlives the gate it took: a leg that gives the gate back before
    /// it ends is still the leg running, and the work it is about to hand on is
    /// dispatched by it alone. [`Claim::take_slot_for_marked`] is what that
    /// outliving is for — a dispatcher tick never sends a job the running leg
    /// is about to send itself.
    leg: Option<Leg>,
    slot: Slot,
}

impl Default for Claim {
    fn default() -> Self {
        Self {
            epoch: 0,
            gate: Gate::Open,
            leg: None,
            slot: Slot::default(),
        }
    }
}

impl Claim {
    pub(super) fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Whether the entry still stands at the epoch a leg carries.
    pub(super) fn stands_at(&self, epoch: u64) -> bool {
        self.epoch == epoch
    }

    /// Move the entry on from the work it stands at. Every leg carrying an
    /// earlier epoch answers for itself alone from here.
    pub(super) fn supersede(&mut self) -> u64 {
        self.epoch += 1;
        self.epoch
    }

    /// Whether anything holds the gate: a claim under way, or a job scheduled
    /// against the entry.
    pub(super) fn is_held(&self) -> bool {
        !matches!(self.gate, Gate::Open)
    }

    /// The job the entry has scheduled and not yet handed to the channel.
    pub(super) fn marker(&self) -> Option<&Job> {
        match &self.gate {
            Gate::Scheduled(job) => Some(job),
            Gate::Open | Gate::Held => None,
        }
    }

    /// Take the gate for the work the entry has just taken on, leaving a job
    /// already scheduled behind it holding the gate in its own right.
    ///
    /// The leg running against the entry is what holds the gate, and where none
    /// is running the job the entry is sending holds it: this is reached from
    /// [`Claim::hand_on`] alone, and what a hand-on sends is in the channel
    /// before the entry's lock goes back. Either way the gate is held across the
    /// move, so nothing takes the entry between the work it moves on from and
    /// the work it moves on to.
    fn hold(&mut self) {
        // A marker holds the gate in its own right.
        if !matches!(self.gate, Gate::Scheduled(_)) {
            self.gate = Gate::Held;
        }
    }

    /// Schedule the entry's next job: the entry moves on to it, and its marker
    /// holds the gate until a dispatcher tick sends it.
    pub(super) fn schedule(&mut self, next: impl FnOnce(u64) -> Job) -> Job {
        let job = next(self.supersede());
        self.gate = Gate::Scheduled(job.clone());
        job
    }

    /// Move the entry on to the next job its running leg sends itself. The leg
    /// holds the gate across the move, so nothing claims the entry between the
    /// work it moves on from and the work it moves on to, and the job it hands
    /// on gets no marker of its own: the leg sends it, and a marker beside a
    /// job already sent is one a dispatcher tick sends a second time.
    ///
    /// A marker left standing from before the move keeps the gate — it names
    /// work raised under the epoch this move supersedes, and [`hand_off`] drops
    /// only a marker naming the job going into the channel.
    ///
    /// [`hand_off`]: Claim::hand_off
    pub(super) fn hand_on(&mut self, next: impl FnOnce(u64) -> Job) -> Job {
        let job = next(self.supersede());
        self.hold();
        job
    }

    /// Put the gate back, and the job scheduled against the entry with it: work
    /// nothing is left holding the entry for is work no dispatch reaches.
    pub(super) fn open(&mut self) {
        self.gate = Gate::Open;
    }

    /// Put back a claim on the gate, leaving a job scheduled against the entry
    /// holding it. The gate stands for a claim and for scheduled work alike, so
    /// a claim gives it back only where nothing is scheduled.
    pub(super) fn release(&mut self) {
        if !matches!(self.gate, Gate::Scheduled(_)) {
            self.gate = Gate::Open;
        }
    }

    /// Give up the job scheduled against the entry, leaving the gate to the leg
    /// running against it and open where none is running.
    ///
    /// The gate the marker gives back is read off the registration: a leg
    /// running against the entry is holding the entry across the marker's end,
    /// and a marker dropped with no leg running is an entry nothing is left
    /// holding.
    pub(super) fn drop_marker(&mut self) {
        if matches!(self.gate, Gate::Scheduled(_)) {
            self.gate = if self.leg.is_some() {
                Gate::Held
            } else {
                Gate::Open
            };
        }
    }

    /// Record the job a claim took the entry from under, where the entry has
    /// nothing else scheduled. The marker holds the gate, so the job the claim
    /// did not take away is one a later tick still reaches.
    ///
    /// A job already scheduled against the entry is the newer of the two and
    /// keeps the gate: the claim is putting back work it interrupted, not work
    /// the entry has since moved on to.
    pub(super) fn restore(&mut self, job: Job) {
        if !matches!(self.gate, Gate::Scheduled(_)) {
            self.gate = Gate::Scheduled(job);
        }
    }

    /// Schedule a job against the entry as it stands, without moving it on.
    ///
    /// Whatever holds the gate gives way to this job, a marker included: the
    /// caller is naming the work the entry owes at the epoch it stands at, and
    /// a marker it displaces is work raised under an epoch the entry has left.
    pub(super) fn mark(&mut self, job: Job) {
        self.gate = Gate::Scheduled(job);
    }

    /// Move the entry on from work an invalidation supersedes. The job it had
    /// scheduled and the slot that job was waiting in go with the epoch they
    /// were raised under; whether the gate itself goes back is the caller's,
    /// because a leg still running is holding it.
    pub(super) fn invalidate(&mut self) {
        self.supersede();
        self.abandon_slot();
        self.drop_marker();
    }

    /// Stop waiting on the job holding the queue slot.
    fn abandon_slot(&mut self) {
        self.slot.abandon();
    }

    /// The leg running against the entry, where one is running.
    pub(super) fn leg(&self) -> Option<Leg> {
        self.leg
    }

    /// Take the entry for a watcher poll: the poll holds the gate and is the leg
    /// running against the entry until its own tick ends it.
    pub(super) fn begin_poll(&mut self, epoch: u64) {
        self.leg = Some(Leg::Poll(epoch));
        self.gate = Gate::Held;
    }

    /// Take the entry for the job leg a worker has begun. The marker the job
    /// stood behind goes with the start: the job is off the channel, and a
    /// marker beside a job already running is one a dispatcher tick sends a
    /// second time.
    pub(super) fn begin_job_leg(&mut self, epoch: u64) {
        self.leg = Some(Leg::Job(epoch));
        self.gate = Gate::Held;
    }

    /// End the claim a watcher poll holds on the entry, where the poll is still
    /// the leg running. A job leg standing at the same epoch is another claim,
    /// and a poll ends its own alone.
    ///
    /// The gate stands for a claim on the entry and for work scheduled against
    /// it alike, so a claim gives the gate back only where nothing is scheduled:
    /// a marker standing holds the gate in its own right.
    pub(super) fn end_poll(&mut self, epoch: u64) {
        if self.leg == Some(Leg::Poll(epoch)) {
            self.leg = None;
        }
        self.release();
    }

    /// End a job leg's hold on the entry, where it is still the leg running. A
    /// leg that handed the entry on is no longer the one running, and what runs
    /// then is that job's own leg.
    pub(super) fn end_job_leg(&mut self, epoch: u64) -> bool {
        if self.leg == Some(Leg::Job(epoch)) {
            self.leg = None;
            true
        } else {
            false
        }
    }

    /// End whichever leg is running against the entry at this epoch. The release
    /// every teardown reaches is the one completion a poll claim and a job leg
    /// come to alike, and it ends the leg it was handed the resources by.
    pub(super) fn end_leg(&mut self, epoch: u64) {
        if self.leg.map(Leg::epoch) == Some(epoch) {
            self.leg = None;
        }
    }

    /// End the leg running against the entry, whatever epoch it stands at.
    ///
    /// [`Claim::leg`] is the leg holding the entry's coverage, so a caller that
    /// takes that coverage out of the entry's own hand takes it out from under
    /// the registration with it: what would otherwise stand is a leg holding
    /// none of the entry's coverage, whose own end would put back a gate the
    /// work that took the coverage is holding.
    pub(super) fn end_running_leg(&mut self) {
        self.leg = None;
    }

    /// Whether a job the entry sent is waiting in its queue slot.
    pub(super) fn slot_taken(&self) -> bool {
        self.slot.taken()
    }

    /// The job waiting in the entry's queue slot, where a job is waiting in it.
    #[cfg(test)]
    pub(super) fn slot(&self) -> Option<u64> {
        self.slot.job()
    }

    /// Take the queue slot for this job, out from under whatever holds it.
    pub(super) fn take_slot(&mut self, epoch: u64) {
        self.slot.take(epoch);
    }

    /// Give back the queue slot a send took, where the entry still holds it for
    /// that send.
    pub(super) fn free_slot(&mut self, epoch: u64) {
        self.slot.free(epoch);
    }

    /// Take the queue slot for the job the entry has scheduled, where no job
    /// holds the slot and no leg is running. The slot is taken under the same
    /// lock that reads the marker, so the job in the channel and the slot naming
    /// it are installed together.
    pub(super) fn take_slot_for_marked(&mut self) -> Option<Job> {
        if self.slot.taken() || self.leg().is_some() {
            return None;
        }
        let job = self.marker()?.clone();
        self.take_slot(job.epoch());
        Some(job)
    }

    /// Stand the entry at an epoch of the caller's choosing. Tests reach states
    /// a race window opens and a deterministic run cannot.
    #[cfg(test)]
    pub(super) fn stand_at(&mut self, epoch: u64) {
        self.epoch = epoch;
    }

    /// Hand the entry the next job a job leg owes it. The leg's claim ends where
    /// the job enters the channel, and the job takes the queue slot under the
    /// same lock, so no producer takes it for newer work in the instant between.
    ///
    /// The job itself holds the gate from here. The marker it stood behind goes
    /// with the send — a marker beside a job already sent is one a dispatcher
    /// tick sends a second time, under the same epoch — while a marker at
    /// another epoch is newer work, and keeps the gate for the tick that sends
    /// it.
    pub(super) fn hand_off(&mut self, leg: u64, job: &Job) {
        self.end_job_leg(leg);
        match self.gate {
            // The leg held the gate across the hand-on, and the job it is
            // sending holds it from here.
            Gate::Held => {}
            Gate::Scheduled(ref marker) if marker.epoch() == job.epoch() => {
                self.gate = Gate::Held;
            }
            Gate::Open | Gate::Scheduled(_) => {}
        }
        self.take_slot(job.epoch());
    }
}
