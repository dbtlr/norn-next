# Vendored dependency patches

Each directory here is one published crate release with a Norn patch applied,
wired in through `[patch.crates-io]` in the workspace manifest.

## `notify`

Patches the `notify` 8.2.0 release from crates.io. The patch adds
an event-history barrier for the FSEvents
backend, in `src/fsevent.rs` and inert on every platform except macOS;
stat-gated content hashing for the poll backend, in `src/poll.rs` and active
on every platform; poll scans that skip unavailable entries; and inotify
coverage that survives denied subdirectories and returns when access does.

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

### Poll scans skip vanished entries

A poll scan lists each directory and then reads each listed entry's metadata.
The release reports every failure on the way as an error event, so an entry
removed between the two reads, such as an editor's temporary file, is an
error, and so is a dangling link when links are followed. The patch skips an
entry below the watched root whose read fails with `NotFound`; the next scan
that does not find a path it held reports the removal. A watched root that is
gone is still reported as an error.

`norn-fs` needs this because a backend error is terminal to a subscription,
so one file deleted in the middle of a tick would end coverage of the vault.

### Denied subdirectories withhold only their own coverage

The recursive inotify install in `src/inotify.rs` skips a non-root directory
whose watch fails at that path, and continues installing readable siblings.
A failed root still refuses installation. Process-resource errors (`EMFILE`,
`ENFILE`, `ENOMEM`, `ENOBUFS`, `EINTR`) and the inotify limit (`ENOSPC`, reported
as `MaxFilesWatch`) still fail coverage.

A recursive parent's `IN_ATTRIB | IN_ISDIR` report for an unwatched child
queues recursive coverage over that child, as a directory creation does.
Restoring access therefore reports the directory and covers later writes
beneath it, including writes in existing nested directories.

Poll scans in `src/poll.rs` also skip denied entries below the watched root.
On Unix, other per-path I/O failures follow the same rule, while the resource
errors above remain backend errors. A denied directory's own stat stays in
the scan, so restoring its mode reports a metadata change. Root scan failures
remain backend errors on every platform.

`norn-fs` needs these changes so one denied directory leaves its subscription
live. The filesystem walk accounts for the unreadable subtree; the watcher
reports restoration and resumes observing its contents.

### Removal

Delete this directory and the `[patch.crates-io]` entry when a published
`notify` release carries equivalents of all these changes.
Where a release carries only some of them, keep the patch and
drop the changes that release made redundant.
