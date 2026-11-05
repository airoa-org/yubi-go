# Contributing

We are not accepting external pull requests yet. Bug reports and questions via Issues are
welcome.

The rest of this file is for maintainers.

## Build and test

[`docs/building.md`](docs/building.md) lists the tools and the build. Before you open a pull
request, run the checks that CI runs:

```bash
cargo build-cwasm
cargo test --workspace --locked
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo ndk -t arm64-v8a -P 29 clippy -p yubi-go --all-targets --locked -- -D warnings
```

The last line checks the Android-only modules, and it needs `cargo-ndk` and the NDK from
`docs/building.md`.

`cargo test` on the host needs a host build of the `airoa_core` payload (`cargo build-cwasm`
with no target argument), which compiles the committed `airoa-sdk/build/airoa-core.wasm`. A
change to `airoa-sdk/python/`, its WIT files, its lock files or `airoa-sdk/build/PAYLOAD`
rebuilds that component from
AIRoA's `airoa_data_collection`. That repository is not public yet, so a maintainer has to make
such a change.

Some modules compile for Android only, so the host tests do not run them. If you change one of
them, say in the pull request how you tested it on a headset.

## Commits

- Use [Conventional Commits](https://www.conventionalcommits.org/): `fix:`, `feat:`, `docs:`,
  `chore:`, with an imperative subject.
- Use body bullets that explain why the change is needed.
- Never commit API keys, passwords, tokens or bucket credentials.

## Licence

By contributing, you agree that your contribution is licensed under Apache-2.0, the licence of
this repository ([`LICENSE`](LICENSE)).
