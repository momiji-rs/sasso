# sasso for Nix

Users don't need anything here: `nix run github:momiji-rs/sasso -- --version`
works because the repo root is a flake. This directory is the packaging itself,
and the notes for keeping it honest.

| file | what it is |
| --- | --- |
| `package.nix` | the `sasso` CLI derivation the root `flake.nix` builds |
| `ffi.nix` | the C ABI as a package — `libsasso.{so,dylib}`, `libsasso.a`, `sasso.h`, a `.pc` file |
| `nixos-vm/` | a throwaway NixOS guest that installs sasso for real and says PASS or FAIL |

The flake exposes both: `packages.sasso` (`default`) and `packages.sasso-ffi`,
plus `overlays.default` so a NixOS or nix-darwin configuration gets `pkgs.sasso`
and `pkgs.sasso-ffi` before either lands in a nixpkgs channel. Rust callers want
neither — they take the crate from crates.io through their own `Cargo.lock`.

## Checking a change

```console
$ nix flake check -L        # what CI runs
$ nix build .#sasso && ./result/bin/sasso --version
$ nix develop               # the toolchain CI uses, dart-sass included
```

`nix flake check` is three derivations, and between them they cover more than a
compile:

- **`sasso`** — the CLI. Building it runs the full `cargo test` suite, because
  the suite needs no network: `tests/parity.rs` shells out to dart-sass only
  under `SASSO_PARITY=1`. `versionCheckHook` then runs `sasso --version` and
  checks it against the version in `Cargo.toml`.
- **`sasso-ffi`** — the C ABI. Its install check drives the *installed* library —
  `libsasso.so` or `libsasso.dylib`, whichever the platform builds — through
  `ffi/examples/smoke.py`, so it tests what a consumer links against rather than
  a build-tree artifact.
- **`dart-compat`** — issue #82's acceptance criteria: the dart-compatible flag
  set (`--no-error-css --stop-on-error --no-color --quiet --quiet-deps
  --style=compressed --no-source-map in.scss:out.css`) compiles, and the bytes
  match what dart-sass emits for the same input. The oracle is nixpkgs'
  dart-sass, pinned by our `flake.lock`, so only a deliberate lock bump can move
  it — which is exactly when a divergence is worth hearing about.

What it cannot cover is the part that needs a machine: a real NixOS activating a
system profile, and `nix run` against a URL from a host that has never seen this
tree. That is `nixos-vm/`.

## The end-to-end run

```console
$ cd nix/nixos-vm
$ sudo ./run.sh                                  # the published flake
$ sudo SASSO_FLAKE=/path/to/checkout ./run.sh    # a local tree
```

Boots a NixOS guest under firecracker (via [microvm.nix]) on a Linux host with
`/dev/kvm`, and checks, in the guest: `sasso` on `PATH` from
`environment.systemPackages` via our overlay, `--version` matching the flake, the
dart-compatible flag set, a stylesheet error exiting non-zero, `pkg-config
--libs sasso` plus a C program that links and runs against `libsasso`, then
`nix run` and `nix profile add` straight from the flake URL. Prints PASS or FAIL
and exits accordingly. A PASS deletes its own scratch directory in `/tmp`, out-link
and guest store image included; a FAIL keeps it and points at the console log.
`KEEP_RUNDIR=1` keeps it either way.

Measured on Linux/x86_64: just under four minutes from boot to verdict, nearly
all of it the guest compiling sasso from the flake URL, plus the host-side build
of the guest itself — seconds when it is already in the store, several minutes
when a changed `package.nix` means rebuilding sasso twice.

Worth running when the flake's *outputs* change shape (a new package, a changed
overlay, a nixpkgs bump that moves `buildRustPackage`), and before a nixpkgs
submission. Not worth running for a code change — `nix flake check` covers that.

[microvm.nix]: https://github.com/astro/microvm.nix

## The nixpkgs copy

nixpkgs carries its own `pkgs/by-name/sa/sasso/package.nix`, and it differs from
`package.nix` here on purpose:

|  | here | nixpkgs |
| --- | --- | --- |
| source | `lib.cleanSource ../.` | `fetchFromGitHub` at the tag |
| version | read from `Cargo.toml` | literal, rewritten by the bot |
| crates | `cargoLock.lockFile` | one `cargoHash` |
| `passthru.updateScript` | none | `nix-update-script { }` |

That shape is not a preference: `nix-update` and the r-ryantm bot that follows
our tags know how to rewrite exactly those fields, and a package they can update
is one nobody has to remember. Keep the judgement calls — the license pair, the
check story, `meta` — identical between the two.

Refreshing it for a new release, from a nixpkgs checkout:

```console
$ nix run nixpkgs#nix-update -- --version 0.14.1 sasso   # src hash + cargoHash + version
$ nix-build -A sasso && ./result/bin/sasso --version
$ nix run nixpkgs#nixpkgs-review -- wip                  # what a reviewer will see
```

Two things nixpkgs asks for that are easy to miss:

- commits are `sasso: init at 0.14.0` / `sasso: 0.14.0 -> 0.14.1` — the prefix
  drives their CI, and a maintainer addition is its own commit
  (`maintainers: add …`, validated by `nix-build lib/tests/maintainers.nix`);
- anything LLM-assisted needs an `Assisted-by:` trailer naming the tool and
  model, per their CONTRIBUTING; `Co-authored-by:` explicitly does not count.
