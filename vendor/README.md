# vendor/

Local forks of third-party crates that Moonshine needs on FreeBSD (and
possibly other BSDs) but that upstream hasn't picked up yet.

Wired into the workspace via `[patch.crates-io]` in the top-level
`Cargo.toml`. Once the upstream projects merge the equivalent changes,
the entries here can be deleted and the workspace patch removed.

## Git dependencies vendored for the port

The `multimedia/moonshine` port pins both the sha256 and the compressed
size of every distfile, but GitHub's codeload endpoint gzips `git
archive` output nondeterministically. The crates upstream fetches from
git are therefore vendored in-tree and referenced by path so they ship
inside the source tarball.

These are verbatim upstream snapshots, not forks — the only edits are to
each `Cargo.toml`, to repoint inter-crate git deps at their vendored
sibling and to strip `[dev-dependencies]` that would otherwise leak into
`Cargo.lock`.

When syncing to a new upstream release, re-vendor any crate whose pin
changed in `moonshine-core/Cargo.toml`:

| crate | pin (upstream v0.15.0) |
| --- | --- |
| `ash` | rev `55dd56906bbb5760e9e9e6c56f45be67f67e0649` |
| `pixelforge` | tag `v0.8.1` |
| `smithay` | branch `master-moonshine` |
| `inputtino` | default branch |

## `socket-pktinfo`

Fork of <https://github.com/pixsper/socket-pktinfo> 0.4.0 with BSD IPv4
PKTINFO support added (`IP_RECVDSTADDR` + `IP_RECVIF` instead of the
Linux-only `IP_PKTINFO` / `struct in_pktinfo`).

Version pinned to `0.3.99` so it satisfies `mdns-sd 0.20.0`'s
`socket-pktinfo = "0.3.2"` requirement via the workspace patch.
