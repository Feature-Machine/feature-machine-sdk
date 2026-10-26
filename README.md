# Feature Machine SDK

Use this repository to make features for Feature Machine. A feature is a Rust library that the
Feature Machine hosts load in the game.

## Crates

| Crate | Use |
| --- | --- |
| `crates/fm_feature_sdk` | The feature API. Your feature links it: implement `Feature` and call `feature!(Type)` once. |
| `tools/fm_pack` | Builds a feature package from one commit (`fm_pack build`). Feature Machine uses the same tool. |
| `crates/fm_input`, `fm_install`, `fm_memory`, `fm_paths`, `fm_platform` | Crates that `fm_pack` needs. |

## Make a feature

1. In the `Cargo.toml` of your feature, set the library type and the SDK at a release tag:

   ```toml
   [lib]
   crate-type = ["cdylib", "rlib"]

   [dependencies]
   fm_feature_sdk = { git = "https://github.com/parallel42/feature-machine-sdk", tag = "v0.1.0" }
   ```

2. Add `feature.json` next to `Cargo.toml`.
3. Commit `Cargo.lock`. The build uses `--locked`.
4. Copy `templates/creator-workflow.yml` to `.github/workflows/feature-machine.yml` in your
   repository. Each push then builds your feature as Feature Machine does, and runs your tests
   and `cargo clippy -D warnings`. The result is for you only: Feature Machine builds again from
   the commit that it reviews.

The build accepts `fm_feature_sdk` only from this repository at a release tag.

## Build a package on your computer (Windows)

```
cargo build --release --locked -p fm_pack
target/release/fm_pack build <your repository> <commit> <feature folder> <output folder>
```

## Rust version

`rust-toolchain.toml` sets the Rust version. The build of your feature uses the same version.

## License

MIT OR Apache-2.0, at your choice (`LICENSE-MIT`, `LICENSE-APACHE`).
