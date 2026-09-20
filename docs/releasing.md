# Release

This repository publishes a Rust binary crate and native archives. `rg` remains a runtime dependency; the Homebrew formula installs it automatically.

1. Update `Cargo.toml` and `Cargo.lock`, and run the checks in README.
2. Run `cargo package --list` and `cargo publish --dry-run --locked`. The package uses an allowlist; real credential files are never included.
3. Commit the release, then push the branch and wait for CI on Linux, macOS and Windows.
4. Run `cargo publish --locked` using the maintainer's crates.io credentials.
5. Tag the tested commit with `v<VERSION>` and push the tag. The Release workflow builds native archives and checksums, then attaches them to a GitHub Release.
6. Update `Formula/jrg.rb` in `sukobuto/homebrew-tap` with the versioned native archive URLs and their SHA-256 values from `SHA256SUMS`. Keep `depends_on "ripgrep"`.
7. Check `brew install sukobuto/tap/jrg` and `brew test sukobuto/tap/jrg`. The custom tap installs the native release binary so users do not need a Rust compiler or Python.

GitHub Releases provide prebuilt binaries for macOS (ARM64 / x86-64), Linux (x86-64 / ARM64, glibc), and Windows (x86-64). Linux binaries target Ubuntu 22.04 or newer. Windows archives contain `jrg.exe`; other platforms contain `jrg`.

CI does not use the TypeSafe key or contact the live API. crates.io publication is a separate explicit maintainer command, so CI needs no registry publishing token.
