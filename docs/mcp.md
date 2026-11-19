# Connecting an MCP client to norn

`norn serve` hosts your registered vaults on a loopback port. Besides its own
wire route, it serves the Model Context Protocol (MCP) at `POST /mcp`. Any
MCP client that speaks Streamable HTTP can list norn's tools and call them. This
page says where to point the client, how it authenticates, and what to expect back.

## Reach the endpoint

The endpoint is `http://127.0.0.1:<port>/mcp`. The host listens on the loopback
address only. The port depends on the channel the host was built for:

| Channel | Default port | State directory name |
|---|---|---|
| Live (released builds) | 28471 | `norn` |
| Dev (development builds) | 28472 | `norn-dev` |

Start the host with `norn serve`. It stays in the foreground. To use another port,
pass `norn serve --port <port>`, or set `port` under `[endpoint]` in the channel's
`config.toml`.

The two channels share no files, so a dev client never reaches a live host's
tokens or port by accident.

## Authenticate with a bearer token

Every request to `/mcp` carries `Authorization: Bearer <secret>`. The secret
comes from the channel's token file.

### Where the file is

| Channel | Path |
|---|---|
| Live | `$XDG_CONFIG_HOME/norn/tokens.toml` |
| Dev | `$XDG_CONFIG_HOME/norn-dev/tokens.toml` |

When `XDG_CONFIG_HOME` is unset or is not an absolute path, use `~/.config` in its
place.

The host creates the file the first time it starts, with one token labelled
`default`. Read the secret from there.

### What the file holds

```toml
version = 1

[tokens.default]
secret = "3f9a1c0b…"
created = 1753900000
```

- `version` is `1`.
- Each `[tokens.<label>]` table is one token. A label is lowercase letters,
  digits, `.`, `_` and `-`, starts with a letter or digit, and is at most 64 bytes.
  Quote labels containing a dot: `[tokens."my.client"]`. Unquoted
  `[tokens.my.client]` creates nested tables; the resulting `my` entry lacks
  `secret`, so the whole token file is refused and every client is locked out.
- `secret` is the token's bytes written as lowercase hex. Send the hex string itself
  as the bearer.
- `created` is the Unix time in seconds that the token was written down.

The file must be mode `0600`, in a directory of mode `0700`. A file or directory
that others can reach is refused, not repaired, because its secrets have already
been exposed.

To give one client a token of its own, add a table by hand until token commands
arrive with the command line interface. A table needs all of its fields:

```toml
[tokens.my-client]
secret = "<output of: openssl rand -hex 32>"
created = <output of: date +%s>
```

Use 32 random bytes (64 hex characters) for the secret, as the host does when it
mints one. The host accepts a shorter secret, but `/mcp` has no rate limit, so a
short secret can be guessed by any local process. The host notices the change and
accepts the new token without a restart.

Edit the file with care. The host reads it whole, and an entry it cannot read, such
as one without `created` or with a secret in upper-case hex, makes every request
from every client fail with `500` and the error `serve/token-file-unreadable` until
the file is fixed. The host log names the entry and what is wrong with it. Fixing
the file restores service without a restart.

### When the bearer is wrong

A missing, stale or revoked bearer gets a plain `401` with a short JSON error body. It
carries no `WWW-Authenticate` header and no OAuth discovery metadata. norn's one kind
of caller authenticates with a static token from a file, and an MCP client that
starts OAuth discovery after a 401 reports a confusing error instead of the real
cause. If a client reports a failed OAuth flow against norn, the token is the
problem: read the file again.

## Add the server to a client

The client needs the URL and the header. Keep the secret out of command lines:
other local users can read a running command's arguments, and the shell keeps them
in its history. Give it to the client through an environment variable instead. For
example, with Claude Code, put this in the project's `.mcp.json`:

```json
{
  "mcpServers": {
    "norn": {
      "type": "http",
      "url": "http://127.0.0.1:28472/mcp",
      "headers": { "Authorization": "Bearer ${NORN_TOKEN}" }
    }
  }
}
```

and set `NORN_TOKEN` in the environment the client starts from, for example from the
dev channel's `default` token (requires Python 3.11 or later for `tomllib`):

```sh
export NORN_TOKEN="$(python3 -c 'import os, tomllib; base = os.environ.get("XDG_CONFIG_HOME", ""); base = base if os.path.isabs(base) else os.path.expanduser("~/.config"); print(tomllib.load(open(os.path.join(base, "norn-dev", "tokens.toml"), "rb"))["tokens"]["default"]["secret"])')"
```

Other clients that take a URL and custom headers work the same way. Keep the secret
out of files you share or commit.

## Protocol revisions

`/mcp` serves two revisions, and it tells them apart message by message:

- **2025-11-25.** The client sends `initialize` first. The host answers with this
  revision's version, its capabilities (tools only) and its name and build. The
  client then sends `MCP-Protocol-Version: 2025-11-25` on every later request. A
  request without that header is refused with a `400`.
- **2026-07-28.** There is no `initialize`. Every request carries the protocol
  version and the client's capabilities in `params._meta`, plus the
  `MCP-Protocol-Version` and `Mcp-Method` headers (and `Mcp-Name` on a tool call).
  `server/discover` answers the supported versions.

Neither revision has a session. The host never issues an `Mcp-Session-Id`, keeps
nothing between requests, and sends no message of its own. Every answer is plain
JSON, never an event stream. A client that sends `Mcp-Session-Id` or `Last-Event-ID`
is served as if it had sent neither.

Also:

- `GET` and `DELETE` on `/mcp` answer `405`. Only `POST` is served.
- A request carrying an `Origin` header is refused with `403`. Browsers are not served.
- A notification, such as `notifications/initialized`, is accepted with `202` and
  has no effect.

## The tools

`tools/list` returns every verb the host serves as a tool, in one page. There are 34
tools, named for the verbs. The list depends only on the build, so it is the same
for every caller and changes only when the host restarts. Under 2026-07-28 it
carries `ttlMs: 300000`, which tells a client it may reuse the list for five minutes.
After a restart onto a new build, a client may see the old list for up to that long.

### Preview and apply

Ten verbs change a vault or the registry, and each has two tools:

- `<verb>` applies the change.
- `<verb>_preview` plans it and writes nothing. It answers with the plan and its
  forecast.

The verbs are `apply`, `set`, `edit`, `new`, `move`, `delete`, `rewrite_wikilink`,
`repair`, `init` and `vault_migrate`. The tool's name decides the mode, so the tools
take no `mode` argument. A call that supplies one is refused.

### Groups and hints

Each tool names a group in `_meta["modgud/groups"]`, a one-element list. A gateway
that sits between a client and norn can use the group, or the hints below, to decide
which tools a caller may see.

| Group | Tools | `readOnlyHint` | `destructiveHint` |
|---|---|---|---|
| `read` | `find`, `search`, `get`, `count`, `validate`, `describe`, `vault_list`, `vault_resolve`, `vault_status`, `doctor_registry`, and every `_preview` tool | true | not set |
| `write` | `new` | false | false (it only creates) |
| `write` | `apply`, `set`, `edit`, `move`, `delete`, `rewrite_wikilink`, `repair` | false | true |
| `admin` | `vault_register`, `vault_reload`, `init` | false | false |
| `admin` | `vault_unregister`, `vault_set`, `vault_migrate` | false | true |

Every tool sets `openWorldHint: false`. No tool sets `idempotentHint`. A preview
tool is in the `read` group even when its verb writes, because it writes nothing.

### Calling a tool

The `arguments` of a call are the verb's native parameters, without `mode`. Each
tool's `inputSchema` describes them. Where a verb accepts one of several shapes,
the schema lists all the properties and the tool's description says which
combination is required, for example "Give exactly one of `path` or `where`." The
host enforces the rule when it reads the call.

A call that succeeds answers with the verb's report in two forms: as
`structuredContent`, and as the same JSON in one text block. A call that fails
answers with `isError: true`. Its `structuredContent` holds the error and its class:
`{"error": {...}, "class": "..."}`. A JSON-RPC error answers only a malformed
message, an unknown tool or method, or bad protocol headers.

Tools declare no `outputSchema`.

## Limits you should know

- **No rate limit on tool calls.** Both revisions say a server must rate limit tool
  invocations. `/mcp` does not, and this is a declared nonconformance. `norn serve` is a
  local, single-user tool behind a bearer. The host does cap the work in flight: it
  answers `503` with `Retry-After: 1` when 64 requests are already running or it
  is draining for a stop.
- **An MCP client cannot verify the host first.** norn's own clients run a hello before
  they send a bearer. The hello proves that the process on the port holds the token, so a
  process squatting on the port cannot collect the bearer. An
  external MCP client cannot run the hello. It reads the token and sends the
  bearer, with no check on who answers
  ([ADR 0054](decisions/0054-a-client-verifies-the-host-before-it-sends-a-bearer.md)
  declares this). Running the host as a service narrows the port-squatting window
  but does not remove it: while the service is stopped or restarting, another local
  user can bind the port first and collect a bearer from an external MCP client.
- **A started call is not cancelled.** If a client disconnects, a call that has not
  started is dropped. A call that has started runs to its end and its answer is
  discarded. `notifications/cancelled` is accepted and ignored.
- **Bodies are bounded.** A request body may be up to 256 MiB and must arrive within
  30 seconds.
