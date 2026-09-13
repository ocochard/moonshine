# vendor/

## Git dependencies vendored for the port

The `multimedia/moonshine` port pins both the sha256 and the compressed
size of every distfile, but GitHub's codeload endpoint gzips `git
archive` output nondeterministically. The crates upstream fetches from
git are therefore vendored in-tree and referenced by path so they ship
inside the source tarball.

These are verbatim upstream snapshots, not forks — the only edits are to
each `Cargo.toml`, to repoint inter-crate git deps at their vendored
sibling, to strip `[dev-dependencies]` that would otherwise leak into
`Cargo.lock`, and to prune optional dependencies for backends moonshine
never builds.

When syncing to a new upstream release, re-vendor any crate whose pin
changed in `moonshine-core/Cargo.toml`:

| crate | pin (upstream v0.16.0) |
| --- | --- |
| `ash` | rev `55dd56906bbb5760e9e9e6c56f45be67f67e0649` |
| `pixelforge` | tag `v0.9.1` |
| `smithay` | rev `0ff00983b6007257a7a161a4fe8b14a778e2ac8f` |
| `inputtino` | default branch (Linux-only, never built here) |

### `smithay`

Upstream v0.16.0 moved off the `hgaiser/smithay` fork to mainline
`Smithay/smithay` at a pinned rev. The vendored copy is that rev with
its `[workspace]` block removed (it lives under this directory's own
workspace root instead) and the optional deps and features for backends
moonshine does not drive — winit, x11 windowing, libinput, udev,
libseat, pixman/glow renderers, vulkan, tracy, libei — pruned away.
Without the prune, Cargo records the full optional-dep union for a path
dependency and `Cargo.lock` gains ~110 never-compiled crates.

`reis` stays a required dependency despite `backend_libei` being pruned:
`src/reexports.rs` re-exports it unconditionally.

### `pixelforge`

Vendored at tag `v0.9.1`. `autoexamples = false` keeps its seven
examples out of the build — they need the stripped `[dev-dependencies]`,
and Cargo's workspace `exclude` does not apply to path dependencies.

## Dropped: `socket-pktinfo`

Previously a fork carrying BSD IPv4 PKTINFO support (`IP_RECVDSTADDR` +
`IP_RECVIF` instead of the Linux-only `IP_PKTINFO` / `struct
in_pktinfo`), wired in through `[patch.crates-io]`.

Upstream <https://github.com/pixsper/socket-pktinfo> merged equivalent
FreeBSD support in 0.4.1, which is what `mdns-sd` now resolves to, so
the fork and the workspace patch were both removed. Reinstate only if
dragonfly/netbsd/openbsd coverage is ever needed — upstream gates on
`target_os = "freebsd"` alone.
