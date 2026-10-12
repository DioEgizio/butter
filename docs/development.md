# Development

```sh
cargo run
cargo test
cargo clippy --all-targets
```

`cargo test -- --ignored build_real_release --nocapture` builds a real
release. Set `BUTTER_TEST_ARCHIVE` to a local source tarball to skip the
download. Set `BUTTER_TEST_CARGO` to use a particular `cargo`.

`cargo test -- --ignored install_real_binary --nocapture` installs a real
release's official build. To skip the download, set `BUTTER_TEST_TOOL_ARCHIVE`
to a local Steam tool archive and `BUTTER_TEST_TOOL_SHA256` to its checksum.
