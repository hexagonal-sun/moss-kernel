{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    fenix.url = "github:nix-community/fenix";
    fenix.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = { self, nixpkgs, fenix, flake-utils }:
    flake-utils.lib.eachDefaultSystem(system:
      let
        pkgs = import nixpkgs { inherit system; };
        toolchain = fenix.packages.${system}.fromToolchainFile {
          file = ./rust-toolchain.toml;
          sha256 = "sha256-SQkt9B1Au5pPWoXg/b190355TVdhBq7xpuWvPaxwM/o=";
        };
        shell = pkgs.mkShell {
          buildInputs = with pkgs; [
            cargo-binutils
            e2fsprogs
            just
            pkg-config
            python3
            pkgsCross.aarch64-embedded.stdenv.cc
            pkgsCross.aarch64-embedded.stdenv.cc.bintools
            qemu
            toolchain
            minicom
            mtools
            wget
          ];
        };
      in
      {
        devShells.default = shell;
      });
}
