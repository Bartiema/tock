# Licensed under the Apache License, Version 2.0 or the MIT License.
# SPDX-License-Identifier: Apache-2.0 OR MIT
# Copyright Tock Contributors 2026.

# Flake for the Nix package manager
#
# This flake provides a development shell with the packages necessary to build
# and flash Tock, mirroring `shell.nix`:
#
#  * rust (as pinned in `rust-toolchain.toml`)
#  * `tockloader`
#
# To use:
#
#  $ nix develop
#

{
  description = "Tock embedded operating system development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    tockloader = {
      # v1.16.0-25-ga865b6f (including nrfutil backend)
      url = "github:tock/tockloader/a865b6f93f63b2af377546e8ad5c85b0a1fd53d1";
      flake = false;
    };
  };

  outputs = { self, nixpkgs, rust-overlay, tockloader }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f (import nixpkgs {
        inherit system;
        overlays = [ rust-overlay.overlays.default ];
      }));
    in
    {
      devShells = forAllSystems (pkgs:
        let
          # Get a custom cross-compile capable Rust install of a specific
          # channel and build. Tock expects a specific version of Rust with a
          # selection of targets and components to be present.
          rustBuild = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;

          tockloaderPkg = import tockloader { inherit pkgs; withUnfreePkgs = false; };
        in
        {
          default = pkgs.mkShell {
            name = "tock-dev";

            buildInputs = with pkgs; [
              # --- Toolchains ---
              llvm
              rustBuild
              openocd

              # --- Convenience and support packages ---
              python3
              tockloaderPkg

              # Required for tools/print_tock_memory_usage.py
              python3Packages.cxxfilt

              # --- CI support packages ---
              qemu
            ];

            LD_LIBRARY_PATH = "${pkgs.stdenv.cc.cc.lib}/lib64";

            # Instruct the Tock gnumake-based build system to not check for
            # rustup and assume all required tools are installed and available
            # in the $PATH
            NO_RUSTUP = "1";

            # The defaults "objcopy" and "objdump" are wrong (stem from the
            # standard environment for x86), use "llvm-obj{copy,dump}" as
            # defined in the makefile
            shellHook = ''
              unset OBJCOPY
              unset OBJDUMP
            '';
          };
        });
    };
}
