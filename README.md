# ShellCraft

ShellCraft is a native terminal for macOS, Windows and Linux, written in Rust. It is in early
development: the repository holds the Cargo workspace and its checks, and the terminal itself is not
built yet. `ROADMAP.md` lists the planned milestones.

Repository: https://github.com/blamejs/ShellCraft

## Build

ShellCraft needs Rust 1.95 or newer.

```sh
cargo xtask ci
```

`ci` runs these steps in order and stops at the first failure:

| Step | What it checks |
|------|----------------|
| `fmt` | `cargo fmt --all -- --check` |
| `clippy` | `cargo clippy --workspace --all-targets -- -D warnings` |
| `test` | the workspace tests, then the tests of the `xtask` package |
| `layers` | each crate depends only on crates in its own layer or a lower one, and the crate manifests follow the rules in `AGENTS.md` |
| `wasm` | the crates that build for the web compile for `wasm32-unknown-unknown` |

Each step also runs on its own, for example `cargo xtask clippy`. The `wasm` step needs the target:
`rustup target add wasm32-unknown-unknown`. No crate builds for the web yet, so `ci` leaves the
`wasm` step out and says so, and `cargo xtask wasm` on its own exits with an error that says the
check did not run. `cargo xtask ci --report <file>` also writes the result
of each step to `<file>` as JSON.

`cargo xtask version` prints the workspace version, and `cargo xtask version set <version>` changes
it in `Cargo.toml` and `Cargo.lock`.

`cargo xtask` builds with `--locked`, so it stops when `Cargo.lock` does not match `Cargo.toml`.
If `version set` stops before it updates `Cargo.lock` and no other `version set` is running, run
`cargo update --workspace`, then run `version set` again.

## License

The code is licensed under either of the MIT license ([LICENSE-MIT](LICENSE-MIT)) or the Apache
License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE)), at your option.

The app icon in `assets/app-icon/` is not covered by that license. It is all rights reserved by
blamejs; see [assets/app-icon/LICENSE.txt](assets/app-icon/LICENSE.txt) for the terms under which
it may be used.
