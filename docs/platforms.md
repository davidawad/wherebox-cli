# Platforms

wherebox-cli supports five targets. There is no CI: run `just test-gate`
before pushing. `just release` builds the release archive for the host it runs
on, and `scripts/publish.sh <tag>` adds another host's archive to an existing
release (see [releasing.md](releasing.md)). The runner column below is where
each target is known to build.

| target | CI runner | toolchain | release archive |
|--------|-----------|-----------|-----------------|
| `x86_64-unknown-linux-gnu` | `ubuntu-latest` (release: `ubuntu-22.04`, glibc 2.35+) | nightly | `.tar.gz` |
| `aarch64-unknown-linux-gnu` | `ubuntu-24.04-arm` (release: `ubuntu-22.04-arm`) | stable | `.tar.gz` |
| `aarch64-apple-darwin` | `macos-latest` | stable | `.tar.gz` |
| `x86_64-apple-darwin` | `macos-15-intel` | nightly | `.tar.gz` |
| `x86_64-pc-windows-msvc` | `windows-latest` | nightly | `.zip` |

GitHub retired the `macos-13` image in December 2025, so Intel macOS runs on
`macos-15-intel`.

## Why nightly on some targets

fsqlite 0.4 turns on unstable compiler features for some targets. This is
true of every 0.4.x release up to and including 0.4.9, the latest:

* **x86_64 (Linux, macOS, Windows).** `fsqlite-pager` and `fsqlite-btree`
  start with `#![cfg_attr(target_arch = "x86_64", feature(core_intrinsics))]`
  and call `core::intrinsics::prefetch_read_data` for L1 prefetch hints in the
  page cache and B-tree cursors.
* **Windows.** `fsqlite-vfs` starts with
  `#![cfg_attr(windows, feature(windows_by_handle))]`, which it uses to
  identify files (volume serial number + file index) for locking.

No fsqlite cargo feature disables either one. (`nightly-simd` in
`fsqlite-types` is a separate, opt-in feature that we leave off.) On aarch64
neither cfg matches, so Linux arm64 and Apple Silicon build on stable.

`rust-toolchain.toml` cannot choose a channel per target, so it pins `nightly`
for everyone, and rustup honours it automatically. To build aarch64 on stable,
run `cargo +stable build` or set `RUSTUP_TOOLCHAIN=stable`. CI does the latter
for its aarch64 jobs. Setting `RUSTC_BOOTSTRAP=1` on a stable compiler would
also get past the feature gates, but the unstable APIs can change without
notice, so we use a real nightly instead. The release archives are prebuilt,
so people installing wherebox (including through Homebrew) need no Rust
toolchain.

## Keys

The default keys work the same everywhere and need no OS service:

* **SSH keys** are read from `~/.ssh` (`%USERPROFILE%\.ssh` on Windows, where
  the built-in OpenSSH client keeps them): `id_ed25519`, then `id_rsa`, or the
  `ssh_key` setting. OpenSSH private keys are parsed by the `age` crate (pure
  Rust), so no `ssh` binary or agent is needed.
* **Key files** live under the config directory and are made owner-only with
  the same code as the database: mode 0600 in a 0700 directory on Unix, a
  protected DACL granting only you and SYSTEM on Windows. A key file other
  users can read is refused on every platform.

There is no OS keychain code: no macOS Keychain, Secret Service (D-Bus) or
Windows Credential Manager dependency, so nothing behaves differently on a
headless server, in a container or in CI.

The test suite never touches `~/.ssh`: SSH keys are generated in-process into
a temporary HOME, and every key test runs on Linux, macOS and Windows in CI.

## File locations

| OS | config file | database |
|----|-------------|----------|
| Linux, BSD | `$XDG_CONFIG_HOME/wherebox-cli/config.toml`, default `~/.config/wherebox-cli/config.toml` | `$XDG_DATA_HOME/wherebox-cli/wherebox.db`, default `~/.local/share/wherebox-cli/wherebox.db` |
| macOS | same as Linux | same as Linux |
| Windows | `%APPDATA%\wherebox-cli\config.toml` | `%LOCALAPPDATA%\wherebox-cli\wherebox.db` |

* **macOS keeps the XDG-style paths.** Every release so far (0.1, 0.2, through
  Homebrew) has stored the database in `~/.local/share/wherebox-cli`.
  Switching to `~/Library/Application Support` would make existing databases
  look missing, so the shipped location stays the default.
* **Windows** uses the known folders (via the `directories` crate): the
  database in the *local* AppData (large, machine-specific), the small config
  file in the roaming AppData. Builds of 0.2 and earlier used `~/.config` and
  `~/.local/share` on Windows too; an existing file there is still found
  (below).
* **Overrides** work everywhere. `XDG_CONFIG_HOME` / `XDG_DATA_HOME` are
  honoured on every OS when set to an absolute path. `WHEREBOX_CONFIG` /
  `--config` and `WHEREBOX_DB` / `--db` override everything. A leading `~/`
  (or `~\` on Windows) expands to `$HOME`, or to `%USERPROFILE%` when `HOME`
  is unset.

wherebox uses no cache directory.

Up to 0.2, wherebox used `~/.config` and `~/.local/share` on every OS. On
Windows those are now legacy: if nothing exists at `%APPDATA%` /
`%LOCALAPPDATA%` but a config or database exists at the old location, the old
one is used, so a new empty database never shadows an existing one
(`src/paths.rs` is the single resolver every command goes through;
`tests/legacy_location.rs` checks it on all platforms).

## Permissions

| | Unix (Linux, macOS) | Windows |
|-|---------------------|---------|
| new database directory | `chmod 0700` | protected DACL: full control for the current user and `SYSTEM`, inherited by children, nothing inherited from the parent |
| files wherebox creates (database, temp file, audit log, lock, `--output`) | created `0600` | the same DACL (non-inheritable) |
| `doctor` check | mode has no group/other bits | every *allow* ACE is for the current user, `SYSTEM`, `Administrators` or the owner placeholders |

The code is in `src/perms.rs`. A directory you created yourself is left alone,
on every OS. `doctor` still reports it and warns when other users can access
it. `SYSTEM` is granted on Windows because backup and indexing services run as
it; it is the counterpart of root, which can read 0600 files anyway. On
volumes without ACL support (FAT/exFAT USB drives) Windows cannot restrict
access at all: wherebox still works there, but `doctor` warns that the files
have no ACL. Keep the database on NTFS.

## Atomic replace, fsync and locking

* **Atomic replace.** The database is written to `<db>.tmp-<pid>`, fsynced and
  renamed over the old file. On Windows `std::fs::rename` replaces an existing
  file, using POSIX semantics where NTFS supports them, so readers that still
  hold the old file open do not block the swap. Directories cannot be opened
  for fsync on Windows, so that step (best effort everywhere) is skipped there.
* **Locking.** `<db>.lock` is locked exclusively with `std::fs::File::lock`,
  which is `flock` on Unix and `LockFileEx` on Windows. No extra crate is
  needed.
* `tests/fs_portability.rs` covers both on every CI platform.

## External tools

wherebox-cli does not start any other program. It does not use minimap2,
samtools, bcftools, gzip, docker or podman. The only shell dependency is in
development: `scripts/readme-session.sh` (the README drift check) needs a
POSIX `sh`. On Windows those tests are reported as *ignored* with that reason;
the Linux and macOS jobs run them.

The genome pipeline lives in **genome-cli**, a separate repository (see
[security.md §7](security.md#7-genome-cli)). This repository cannot change its
process spawning. These are the requirements it should follow, and how its
tools run on each OS:

* **Resolving executables.** Look tools up with the `which` crate
  (`which::which("samtools")`), which applies `PATHEXT` and finds
  `samtools.exe` on Windows. Spawn the resolved path with `std::process::Command`,
  never through `sh -c`, and fail with a message that names the missing tool.
  Tests that need a tool should skip with a message when it is not found.

| tool | Linux / macOS | Windows |
|------|---------------|---------|
| minimap2 | native (bioconda, Homebrew, distro packages) | **WSL2**. There is no maintained native build, and bioconda has no `win-64` packages. |
| samtools, bcftools (htslib) | native | **WSL2**. MSYS2/Cygwin builds exist but are untested and unsupported. |
| gzip | native | native via `gzip.exe` from Git for Windows. In-process decompression (e.g. the `flate2` crate) avoids the dependency. |
| docker | native | native via Docker Desktop (WSL2 backend). Bind-mount Windows paths with forward slashes. |
| podman | native | native via Podman Desktop / `podman machine` (WSL2 VM) |

In practice, run the alignment and variant-calling pipeline either inside WSL2
(install genome-cli's Linux build there) or inside a container from native
Windows. wherebox itself runs natively on Windows.
