# norn

Deterministic tooling for querying, validating, and maintaining Markdown vaults
through user-defined vault schemas. Norn keeps a vault consistent — valid
frontmatter, accurate queries, planned mutations — so humans and agents can share
one vault without drift.

**Status:** early development; no releases yet.

- [Architecture](docs/architecture.md) — the invariant spine and crate map.
- [Decisions](docs/decisions/README.md) — the ADR index.

## Development process recovery

`scripts/norn-process` runs Norn's non-shipping process-supervision tool. The
launcher finds the repository from its own path, so callers can use it from any
working directory.

```sh
./scripts/norn-process reap
./scripts/norn-process report --since-unix-ms 1787266800000
```

`reap` writes one JSON object to stdout. The object contains `found`, `cleaned`,
`refused`, and `errors` counts, plus one entry for each stale registration. A
completed operation exits with status 0, including an operation that cleans or
refuses a group. A group-level error stays in the JSON and produces a nonzero
status. A command-boundary error produces a nonzero status and writes a
diagnostic to stderr.

`report` writes the durable audit events. Without `--since-unix-ms`, the report
contains all events and sets `since_unix_ms` to `null`. With the option, the
report contains events at or after the supplied Unix time in milliseconds and
echoes that value in `since_unix_ms`.

Automation owns the schedule, the report cutoff, and any message produced from
the JSON. The launcher and the `norn-process` binary do not enter Norn release
packages.

## License

[MIT](LICENSE)
