# nros-cli — moved into the nano-ros monorepo (Phase 218)

**This repository is now read-only.** As of 2026-06-04 (Phase 218 of
the nano-ros project), the `nros` CLI ships from inside the
[NEWSLabNTU/nano-ros](https://github.com/NEWSLabNTU/nano-ros) monorepo
as the sub-workspace at
[`packages/cli/`](https://github.com/NEWSLabNTU/nano-ros/tree/main/packages/cli).
The two-repo split (which began with Phase 195.D) is closed because
the CLI codegen format and the runtime ABI must move together — the
`NROS_VERSION=0.3.7` pins that proliferated across the nano-ros
workflows were the visible scar of that split.

The repository is preserved here for history (the original codegen
carve-out, the `nros ws sync` design discussions, the `nros plan`
launch-resolver lineage, and the 0.3.x release tag set). Issue / PR
history stays browseable.

## Get the CLI today

```sh
git clone https://github.com/NEWSLabNTU/nano-ros.git
cd nano-ros
source ./activate.sh    # or: direnv allow / source ./activate.fish
just setup-cli          # builds packages/cli/target/release/nros
```

After `just setup-cli`, the `nros` binary lands at
`packages/cli/target/release/nros` and `activate.sh` puts it on PATH
ahead of the legacy `~/.nros/bin/nros` location. Verify with:

```sh
nros --version
# expected output: nros 0.4.0   (Phase 218 monorepo-merge baseline)
```

### From a tagged release (no Rust toolchain)

```sh
# inside a nano-ros checkout at a `nros-v<X.Y.Z>` tag:
./scripts/install-nros-prebuilt.sh
```

Fetches `nros-<triple>.tar.gz` from the matching GitHub release,
sha256-verifies, installs at `packages/cli/target/release/nros`.

## What changed?

- **Source tree:** every former `packages/<crate>/` under this repo is
  now at `packages/cli/<crate>/` in nano-ros. The lift used
  `git filter-repo --to-subdirectory-filter packages/cli/` and is
  squash-merged onto nano-ros `main`.
- **Versioning:** the CLI and the runtime now share a single bundle
  version (JetPack-style). See
  [`docs/development/versioning.md`](https://github.com/NEWSLabNTU/nano-ros/blob/main/docs/development/versioning.md).
- **ABI guard:** `nros generate-rust` / `nros codegen` now read the
  consumer's `Cargo.lock`, compare the resolved `nros-core` version
  to the CLI binary's compile-time version, and reject mismatches.
  `NROS_SKIP_VERSION_CHECK=1` opts out.
- **Distribution:** no crate publishes to crates.io. Tagged releases
  ship the CLI binaries (four target triples: linux+macos × x86_64+
  aarch64) as GitHub release assets; runtime crates are consumed via
  path-deps / `[patch.crates-io]` redirects (the `nros ws sync` verb
  writes those).

## File issues / PRs against nano-ros

All future CLI work happens in
[NEWSLabNTU/nano-ros](https://github.com/NEWSLabNTU/nano-ros). The
CLI sub-workspace lives at
[`packages/cli/`](https://github.com/NEWSLabNTU/nano-ros/tree/main/packages/cli).
When filing issues there, the `cli` label scopes them to the
sub-workspace.

## Historical references

- [Phase 195.D — original carve-out](https://github.com/NEWSLabNTU/nano-ros/blob/main/docs/roadmap/archived/phase-195-just-to-nros-cli.md)
  (the move OUT of the monorepo).
- [Phase 218 — monorepo merge](https://github.com/NEWSLabNTU/nano-ros/blob/main/docs/roadmap/phase-218-merge-cli-into-monorepo.md)
  (the move BACK).
- [Phase 218 design spec](https://github.com/NEWSLabNTU/nano-ros/blob/main/docs/superpowers/specs/2026-06-04-cli-monorepo-merge-design.md).

The last standalone release tag was
[`nros-v0.3.7`](https://github.com/NEWSLabNTU/nros-cli/releases/tag/nros-v0.3.7);
the post-merge release line continues from `nros-v0.4.0` in the
nano-ros repo.

## License

MIT OR Apache-2.0. See [LICENSE-APACHE](LICENSE-APACHE) /
[LICENSE-MIT](LICENSE-MIT).
