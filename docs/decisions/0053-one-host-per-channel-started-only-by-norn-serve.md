---
status: accepted
date: 2026-11-13
---

# 0053 — One host per channel, started only by `norn serve`, mints its own credential

## Context

One host process serves every registered vault of a channel, and it must start three ways:
by a person or an agent harness in a terminal, by the service manager, and by the CLI when
no host answers. Every way has to agree on which process serves the channel, where it
listens and what credential a caller presents, and none of them may depend on a file the
host writes for callers to find it, because a host that crashes leaves such a file stale.

The shape drawn before the serving layer was built left gaps. A supervised service and an
auto-launched instance each reached a host entry point that was not a public command, so a
host could not be run by hand in the foreground. The bearer token was generated at service
install, so a host that was never installed as a service had no credential. And nothing but
the port kept two hosts of one channel apart, while a port can be held by any process.

## Decision

**`norn serve` is the one host entry point.** It is a public command that runs the
channel's one host process in the foreground. The service manager's unit and the CLI's
auto-launch run that same command, and there is no hidden or development-only host entry
beside it. It is a process entry, not a wire request, so it carries no request scope. Who
started it sets its lifetime: an explicit host, started by hand or by the service manager,
never exits on its own; an auto-launched host, marked by a `serve` argument the CLI passes,
always binds its channel's resolved endpoint and exits after an idle timeout, closing its
listener before it drains.

**One host serves a channel, under a channel lock that records nothing.** A starting host
takes a lock file in the channel's machine-local state, owned by the configuration crate,
before it binds the port or does any other start-up work, and holds it for its lifetime.
It takes the lock without blocking, so a contended start decides instead of hanging. The
lock says that a host serves the channel, not where. A start learns the rest by dialing
its channel's endpoint, a convention resolved from the channel's default port or the
machine settings file, and verifying whatever answers there before it sends a credential.

**An explicit start takes over an auto-launched host and never displaces an explicit
one.** An explicit start that finds the lock held by an auto-launched host asks that host,
over the authenticated wire route, to drain and exit, then takes the lock and the port
once they are free. An explicit start that finds an explicit host refuses and exits
nonzero. A launched start that finds a host answering exits at once with a status that
says a host serves the channel, and the CLI that launched it dials again. Every wait on a
held lock is bounded by a release bound that is a constant of the channel, independent of
either process's settings, and every stopping host frees the lock within that same bound
of closing its listener. A start that holds the lock and does not reach listening within a
start bound gives the lock back.

**The host mints its own bearer credential at first start.** Whenever `norn serve` starts
and the channel's token file holds no token, it mints exactly one from the operating
system's random source and writes it create-if-absent under the token file's lock, so two
hosts starting at once mint one token. The host is the only mint site. Service install
touches no token, because its unit's first start mints where the file is empty; the client
mints nothing; and later token management is a request to the host, which mints the bytes
and stores them. The configuration crate stores tokens and holds no randomness. Every
stored token is valid, and a label names one token so it can be revoked.

## Consequences

- **One launch path.** A foreground host, the service and an auto-launched host share
  start-up, takeover, credential and logging, so a defect in one is a defect in all and is
  found by the same tests.
- **A host has a credential however it started.** A foreground host serves before any
  service is installed, and an external client finds the credential in the token file,
  whose path and format become a documented external contract.
- **A start cannot find a host serving another port.** After the configured port changes,
  or under a settings file naming another, while an auto-launched host serves on the old
  port, an explicit start finds the lock held and nothing at its endpoint, and refuses past
  the release bound until that host idles out. That gap is accepted.
- **The channel lock is the configuration crate's one non-blocking lock.** Its other locks
  block around one read-modify-write of a small file.
- **The release bound is not a setting.** A drain setting is capped below it, so a waiter
  never gives up on a host still releasing under a longer drain than its own.
- **Self-update reuses the takeover.** A new CLI replaces an older auto-launched host; an
  older explicit host is never stopped, and the CLI says to restart it.

## Alternatives

**`serve` as a hidden or development-only entry.** The service unit and auto-launch still
need a host entry point, so a hidden one is a second, undocumented entry beside the public
commands, and a person who wants a host in a terminal has none. A per-vault serving process
stays out either way; what this decision adds is a public name for the one-host process.

**An endpoint file, or a discovery handshake.** The host would write where it listens for
clients to read. That is state a crashed host leaves stale, a second writer of machine-local
state, and a file a client must trust. A channel default with an override in a settings file
both sides read reaches the same agreement with nothing written.

**A lock that records the holder's port.** It would let a start find a host serving another
port and close the gap above, but it makes the lock an endpoint file with the same staleness,
and a recorded port is a claim any process able to write the lock can make. A start that
dials the convention and verifies what answers needs nothing recorded.

**Minting the credential at service install.** A host never installed as a service has no
credential, and a foreground host and a service would need two mint paths.

**Minting in the client.** The host would start without a credential until a client made
one, and the decision that a credential exists would sit in a process that does not need it
to serve. Two mint sites would also split where a credential's quality is decided. The
client draws only a verification nonce.
