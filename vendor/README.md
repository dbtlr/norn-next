# Vendored dependency patches

Each directory here is one published crate release with a Norn patch applied,
wired in through `[patch.crates-io]` in the workspace manifest.

## `notify`

Patches the `notify` 8.2.0 release from crates.io. Every change is in
`src/fsevent.rs`, so the patch is inert on every platform except macOS.

The capability the patch adds is an event-history barrier for the FSEvents
backend:

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
- Committing a non-empty path set reports a stream that failed to start, where
  the release discards that error.

`norn-fs` needs these to publish watcher readiness only after coverage is
provably continuous from before its edges were installed. The release exposes
no event identifier and drops the sentinel, so the boundary is unobservable
through its public API.

Delete this directory and the `[patch.crates-io]` entry when a published
`notify` release carries an equivalent capability.
