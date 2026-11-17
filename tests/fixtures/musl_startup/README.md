# Isolated x86_64 musl startup regression

This dependency-free executable isolates the `musl-gcc`/static-PIE interaction
from Codr, Tokio, OpenSSL and Odin. Use the repository's pinned Rust toolchain,
an installed `x86_64-unknown-linux-musl` target, and native `musl-gcc`.
Run Cargo through the configured build-storage adapter. No account or service is
opened, and the fixture is not a release artifact.

With the musl GCC specs that inject an interpreter for `-static-pie` (reproduced
locally with musl 1.2.3-1), the old packaging configuration fails:

```sh
CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=musl-gcc \
  cargo run --locked --release --target x86_64-unknown-linux-musl \
  --manifest-path tests/fixtures/musl_startup/Cargo.toml
```

With the musl interpreter installed this exits on SIGSEGV (shell status 139).
Without that interpreter it cannot execute (`ENOENT`). `readelf -l` on the
Cargo-reported executable shows `INTERP` requesting `/lib/ld-musl-x86_64.so.1`.

The repaired configuration retains PIE and removes that interpreter:

```sh
CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=musl-gcc \
  CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_RUSTFLAGS='-C link-arg=-Wl,--no-dynamic-linker' \
  cargo run --locked --release --target x86_64-unknown-linux-musl \
  --manifest-path tests/fixtures/musl_startup/Cargo.toml
```

It prints `musl startup passed` and exits zero. `readelf -h` still reports `DYN`
(PIE), and `readelf -l` has no `INTERP`. These isolated checks do not replace the
application's native package smokes, connector checks or platform qualification.
