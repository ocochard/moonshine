# vendor/

Local forks of third-party crates that Moonshine needs on FreeBSD (and
possibly other BSDs) but that upstream hasn't picked up yet.

Wired into the workspace via `[patch.crates-io]` in the top-level
`Cargo.toml`. Once the upstream projects merge the equivalent changes,
the entries here can be deleted and the workspace patch removed.

## `socket-pktinfo`

Fork of <https://github.com/pixsper/socket-pktinfo> 0.4.0 with BSD IPv4
PKTINFO support added (`IP_RECVDSTADDR` + `IP_RECVIF` instead of the
Linux-only `IP_PKTINFO` / `struct in_pktinfo`).

Version pinned to `0.3.99` so it satisfies `mdns-sd 0.20.0`'s
`socket-pktinfo = "0.3.2"` requirement via the workspace patch.
