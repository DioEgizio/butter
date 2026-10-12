<img width="290" height="240" alt="butter dog" src="https://github.com/user-attachments/assets/8d789a20-f396-442d-9e48-d980e2fc775b" />

# Butter

Butter manages [Rosemary](https://github.com/microlift/rosemary), a
compatibility layer that runs macOS applications on Linux, the way
[Bottles](https://usebottles.com) manages Wine. It is a GTK 4 and libadwaita
application written in Rust.

- **Runners** are builds of Rosemary. Butter lists the tagged releases from
  Rosemary's GitHub repository. It installs the official build of the one you
  choose, or compiles it from source. You can also add a `rosemary` binary you
  built yourself.
- **Bottles** are Rosemary prefixes: each has its own Darwin filesystem (with
  the verified MiniDarwin base), its own runner, and its own settings and
  environment variables.
- **Programs** are `.app` bundles or Mach-O executables saved in a bottle, so
  they run with one click. You can also run one once without saving it.
- The **Process Manager** lists everything running in a bottle. Each program
  gets a terminal of its own: you can read its output, type into it, send it
  signals and stop it. The power button in a bottle's header stops the whole
  bottle.

## Install

Butter supports three installations officially: Flatpak, Nix and building from
source.
Either can install official Rosemary builds, which need nothing beyond an
OpenGL or Vulkan driver, or you can compile runners on your computer.

### Flatpak

Each [release](https://github.com/microlift/butter/releases) has a
`Butter-<version>-x86_64.flatpak` bundle. Download it and install it:

```sh
flatpak install --user Butter-<version>-x86_64.flatpak
flatpak run io.github.microlift.butter
```

The bundle points at Flathub for the GNOME runtime it needs, so
[Flathub must be set up](https://flathub.org/setup) first. To check the
download, compare it with `SHA256SUMS` from the same release, or verify its
build provenance with `gh attestation verify <file> --repo microlift/butter`.

To build the Flatpak yourself instead:

```sh
flatpak install flathub org.gnome.Sdk//51 org.freedesktop.Sdk.Extension.rust-stable//26.08
flatpak-builder --user --install --force-clean build-dir build-aux/io.github.microlift.butter.yml
flatpak run io.github.microlift.butter
```

To let the Flatpak see a folder of programs, and for details on optional
permissions, see [Flatpak](docs/flatpak.md).

### Nix

The repository is a Nix flake for `x86_64-linux` and `aarch64-linux`:

```sh
nix run github:microlift/butter
```

To install it permanently, add it to a standard flake-based NixOS
configuration or to Home Manager. See [Installing with Nix](docs/building.md#installing-with-nix)
for both.

### From source

```sh
make
make install PREFIX=~/.local     # or: sudo make install
```

See [Building from source](docs/building.md) for dependencies, Nix and NixOS.

AppImage support is planned.

## Getting started

1. Open **Runners** and install a Rosemary release.
2. Create a bottle and choose that runner.
3. Add a `.app` bundle or Mach-O executable and run it.

## Documentation

- [Flatpak](docs/flatpak.md): sandbox permissions and onboarding
- [Building from source](docs/building.md): dependencies and Nix
- [Where things are](docs/data-layout.md): runners, bottles, settings
- [How Butter works](docs/how-it-works.md): running programs, installing and building runners
- [Development](docs/development.md): running tests

## License

GPL-3.0-or-later
