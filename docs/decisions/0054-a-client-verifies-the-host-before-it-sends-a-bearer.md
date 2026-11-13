---
status: accepted
date: 2026-11-13
---

# 0054 — A client verifies the host before it sends a bearer

## Context

A host listens on its channel's loopback port, and that port is a convention, not a
resource the host always holds. While no host holds it — after an auto-launched host's idle
exit and before the next launch, or before any host has started — another local user can
bind it. A client that dials the convention and sends its bearer to whatever answers hands
the credential to that process, which can then use it against the real host. Loopback keeps
remote peers out; it does not keep out other users of the same machine. A squatter can also
relay whatever a client sends to a real host serving the channel on another port, so a check
the real host answers on the squatter's behalf proves nothing about who holds the dialed
port.

## Decision

**A client sends the hello before its bearer.** The hello carries the label of the token
the client holds and a random nonce. The host answers with its channel, its build identity,
its launch kind and the port it is bound to, and an HMAC keyed by that one labelled token
over the nonce, the channel and that port. The client refuses unless the HMAC verifies and
the reported port is the port it dialed. It never sends its bearer after a failed hello,
and its report names both possible causes without asserting either: the credential may
have changed since the client read it, or something other than a norn host holds the port.

**Every connection is verified.** Every dialer — the CLI, the stdio-MCP shim, a starting
host's takeover dial and self-update's takeover — runs the hello as the first exchange of
each connection it opens, and a reopened or pooled connection is no exception, so no
connection carries a bearer without its own verified hello. A dialer connects only to a
resolved loopback socket address, never to a hostname.

**The hello is the one exchange admitted without a bearer.** It carries no request and
reveals nothing a local peer could use, and answering it costs one token lookup by label,
whatever the number of stored tokens. Its shape is versioned and kept stable across
releases, so a newer client can verify, and retire, an older host.

**External MCP clients cannot verify the host, and that limitation is declared.** An HTTP
MCP client reads the secret from the channel's token file and sends a plain bearer with no
hello. On a machine shared with an untrusted local user, the host should run as a service,
so the port stays held.

## Consequences

- A squatter without the token cannot answer the hello. One that relays it to a real host on
  another port returns an answer bound to that port, and the client refuses it.
- A token rotated between a client's read of the file and its hello fails verification and
  withholds the bearer; the client cannot tell that from a squatter, and says so.
- Every connection pays one exchange before its first request, which a fresh-connection
  clock measures.
- The nonce and the HMAC are third-party dependencies of the client and the serving crate,
  not workspace edges.
- The CLI and the shim are protected against a port squatter; external MCP clients are not.

## Alternatives

**Declaring other local users out of scope.** The serving side would need no handshake. But
an auto-launched host's idle exit frees the port on an ordinary schedule, so every CLI
invocation after one would hand its bearer to whatever bound the port first. On a shared
machine that is a credential leak in normal operation, not an edge case.

**A Unix-domain socket for the CLI.** File permissions on the socket would authenticate the
host to the client with no exchange. But external MCP clients need loopback HTTP regardless,
so the socket would be a second transport beside it, with routing, authentication and their
bars built and held twice, and it would protect no external client either. One transport and
one challenge keep one serving path.
