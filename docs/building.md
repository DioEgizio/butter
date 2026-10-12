# Building from source

You need Rust 1.88 or newer, GTK 4.12 or newer, libadwaita 1.5 or newer, and
the GTK 4 build of VTE.

```sh
# Debian and Ubuntu
sudo apt install build-essential pkg-config libgtk-4-dev libadwaita-1-dev libvte-2.91-gtk4-dev
# Fedora
sudo dnf install gcc pkgconf-pkg-config gtk4-devel libadwaita-devel vte291-gtk4-devel

make
make install PREFIX=~/.local     # or: sudo make install
```

Official builds need nothing more. To build runners from source, you also
need Rosemary's own build dependencies. The Runners page checks for them automatically.

## Nix

The flake provides the package, a check suite and a development shell:

```sh
nix run github:microlift/butter    # run without installing
nix build                            # build ./result/bin/butter
nix develop                          # shell with Rust, GTK 4, libadwaita, SDL 3
nix flake check                      # build, clippy and rustfmt
```

On NixOS, official Rosemary builds are prebuilt binaries, so enable
[`nix-ld`](https://search.nixos.org/options?query=programs.nix-ld) to run them.

### Installing with Nix

Butter is a flake, so add it as an input and install
`butter.packages.${system}.default`. The examples assume the input is named
`butter`; add `inputs.nixpkgs.follows = "nixpkgs"` if you want it to share your
nixpkgs (the flake pins its own Rust toolchain through `rust-overlay` either way).

#### Standard flakes (NixOS)

```nix
# flake.nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    butter.url = "github:microlift/butter";
  };

  outputs = { nixpkgs, butter, ... }: {
    nixosConfigurations.myhost = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        ({ pkgs, ... }: {
          environment.systemPackages = [
            butter.packages.${pkgs.stdenv.hostPlatform.system}.default
          ];
        })
      ];
    };
  };
}
```

Then run `sudo nixos-rebuild switch --flake .#myhost`. Without NixOS, use
`nix profile install github:microlift/butter`.

#### Home Manager

With Home Manager as a flake module (or as a NixOS module), pass the flake
inputs through and add the package to `home.packages`:

```nix
# flake.nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    home-manager = {
      url = "github:nix-community/home-manager";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    butter.url = "github:microlift/butter";
  };

  outputs = { nixpkgs, home-manager, butter, ... }: {
    homeConfigurations."rachel" = home-manager.lib.homeManagerConfiguration {
      pkgs = nixpkgs.legacyPackages.x86_64-linux;
      extraSpecialArgs = { inherit butter; };
      modules = [ ./home.nix ];
    };
  };
}
```

```nix
# home.nix
{ pkgs, butter, ... }:
{
  home.packages = [ butter.packages.${pkgs.stdenv.hostPlatform.system}.default ];
}
```

Apply it with `home-manager switch --flake .#alice`. The package installs the
desktop file and icons, so Butter appears in your application launcher.
