{
  description = "herdr — terminal workspace manager for AI coding agents";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    flake-parts.url = "github:hercules-ci/flake-parts";

    crane.url = "github:ipetkov/crane";

    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    treefmt-nix = {
      url = "github:numtide/treefmt-nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    flake-compat = {
      url = "github:inclyc/flake-compat";
      flake = false;
    };
  };

  outputs =
    inputs@{ flake-parts, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];

      imports = [
        inputs.treefmt-nix.flakeModule
        ./nix/modules/packages.nix
        ./nix/modules/overlays.nix
        ./nix/modules/devshells.nix
        ./nix/modules/treefmt.nix
        ./nix/modules/hm-module.nix
      ];
    };

  nixConfig = {
    # Uncomment and fill in after creating the cache on cachix.org:
    # extra-substituters = [ "https://herdr.cachix.org" ];
    # extra-trusted-public-keys = [ "herdr.cachix.org-1:YOUR_PUBLIC_KEY_HERE" ];
  };
}
