# cargo-nano-ros

Compatibility Cargo subcommand for [nano-ros](https://github.com/NEWSLabNTU/nano-ros) message generation. The canonical user CLI is `nros`; install it from the `nros-cli` crate and run `nros generate-rust`.

```bash
cargo install nros-cli
nros generate-rust --force
```

`cargo nano-ros generate-rust` remains available for older Cargo workflows, but new scripts and docs should use `nros generate-rust`.

## License

Licensed under either of [Apache-2.0](https://www.apache.org/licenses/LICENSE-2.0) or [MIT](https://opensource.org/licenses/MIT) at your option.

Part of the [nano-ros](https://github.com/NEWSLabNTU/nano-ros) project.
