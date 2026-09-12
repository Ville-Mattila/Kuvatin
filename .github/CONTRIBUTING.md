# Contributing

Thanks for looking. Kuvatin is a small, opinionated Windows app maintained by
one person; issues and pull requests are welcome, and the notes below are what
will be asked about anyway.

## Reporting something

Use the issue templates — they ask for the version, the Windows build and what
you did, which is most of what a diagnosis needs. The log at
`%LOCALAPPDATA%\Kuvatin\kuvatin.log` records right-click runs, menu
registration and update checks; attaching it usually saves a round trip.

For anything security-sensitive, see [SECURITY.md](SECURITY.md) instead.

## Before opening a pull request

Open an issue first for anything larger than a fix. Kuvatin deliberately does a
few things well — batch image conversion and a light video timeline — and a
feature that does not fit that shape is likely to be declined after the work is
done, which is a waste of your evening.

Windows is the only supported platform. A macOS port existed once and was
dropped for lack of a machine to test it on; please do not reintroduce
cross-platform scaffolding.

## Building

You need the MSVC Rust toolchain (the version is pinned in
`rust-toolchain.toml`; rustup picks it up), the Visual Studio C++ build tools
(the `webp` crate compiles libwebp), and the **GStreamer MSVC runtime and devel
MSIs** at their default install path — `.cargo/config.toml` points the build at
them. The exact GStreamer version CI uses is in
`.github/workflows/release.yml`.

```powershell
cargo build --release      # everything
cargo run -p kuvatin       # the GUI
```

## Tests

```powershell
cargo test -p kuvatin-core -p kuvatin            # deterministic, no media
cargo test -p kuvatin-video -- --test-threads=1  # drives real pipelines
```

The video suite must be single-threaded: concurrent GES pipelines deadlock. It
also needs GStreamer's `bin` directory on `PATH`.

New behaviour comes with a test where there is a seam for one. Most of this
codebase was written test-first, and the tests are named as sentences about
what should be true (`a_cancelled_run_is_silent_even_when_files_had_already_failed`)
rather than after the function they cover.

## What CI will check

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -D warnings`,
`cargo deny check` (licences and advisories), the two test suites above, and a
smoke render with the built executable. A pull request that touches the
installer, the menu package or the shell integration also builds the MSI and
installs it on the runner.

Run the first three locally before pushing; they are quick and they are the ones
that fail most often.

## Style

Match the file you are editing. Two habits are consistent throughout and worth
keeping:

- **Comments explain why, not what.** If a line looks odd, the comment says what
  went wrong without it. Several of them are the only record of a bug that took
  a day to find.
- **Measure before optimising, and write down the measurement.** There are
  several comments in the image pipeline that exist to stop the next person
  "fixing" something that was already tried.

Commit messages: a short imperative subject line, then prose explaining what was
wrong and what changed. No prefixes or ticket numbers.

## Licence

Kuvatin is GPL-3.0-or-later. By contributing you agree that your contribution is
licensed the same way.
