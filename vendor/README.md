# Vendored dependency patches

Each directory here is one published crate release with a Norn patch applied,
wired in through `[patch.crates-io]` in the workspace manifest.

## `notify`

Patches the `notify` 8.2.0 release from crates.io. The patch adds two
capabilities: an event-history barrier for the FSEvents backend, in
`src/fsevent.rs` and inert on every platform except macOS, and stat-gated
content hashing for the poll backend, in `src/poll.rs` and active on every
platform.

### FSEvents event-history barrier

The first capability is an event-history barrier for the FSEvents backend:

- `fsevent::current_event_id()` reads the per-host FSEvents event identifier.
- `FsEventWatcher::with_event_history(handler, since_when, history_done)`
  creates a watcher whose stream replays every event recorded after
  `since_when` instead of starting at the moment the stream is created, and
  which calls `history_done` when that replay is complete.
- The `HistoryDone` sentinel reaches `history_done` instead of being discarded.
  It is a stream marker, not a filesystem event, so it never reaches the event
  handler.

Two refusals come with it, because the barrier makes them reachable:

- A `FSEventStreamCreate` that returns no stream is an error, where the release
  schedules the null pointer.
- A `FSEventStreamStart` that returns false is an error, and it reaches the
  caller: `commit`, `watch` and `unwatch` all report a non-empty path set whose
  stream did not start, where the release discards both the Boolean and the
  result of the call that reads it. Without this, a watcher that observes
  nothing is indistinguishable from a quiet filesystem until the barrier's
  deadline.

One deletion comes with it:

- Stream teardown no longer calls `FSEventsPurgeEventsForDeviceUpToEventId`.
  The release purges the device's event log to suppress callbacks for events
  still pending on a stopped stream. That log is what the barrier replays
  from, so purging it lets one subscription's teardown discard the interval
  another subscription in the same process is replaying.

`norn-fs` needs these to publish watcher readiness only after coverage is
provably continuous from before its edges were installed. The release exposes
no event identifier and drops the sentinel, so the boundary is unobservable
through its public API.

### Stat-gated poll hashing

The second capability bounds what a poll tick reads. With content comparison
enabled, the release reads and hashes every watched file on every tick, and
compares modification times only to the whole second. The patch changes that:

- Each tick compares size, modification time, change time and inode, the last
  two on Unix only, at full timestamp precision.
- Content is read only for an entry whose stat changed or that is new. An
  entry whose stat is unchanged keeps its previous hash, so a tick over an
  unchanged tree reads no file content.
- A stat change is reported even where the content hash is unchanged.
- `PollWatcher::content_reads()`, hidden from the documentation, counts the
  content hashes the watcher has attempted, initial scans included.

`norn-fs` needs this so a polled vault costs stat calls per tick, not its
whole size in reads. The release has no stat gate on its content comparison
and no way to observe how much content a tick read.

### Removal

Delete this directory and the `[patch.crates-io]` entry when a published
`notify` release carries equivalents of both capabilities. Where a release
carries only one, keep the patch and drop the changes that release made
redundant.
