{
  description = "herdr — terminal workspace manager for AI coding agents";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    # Shareable code-quality / governance gates + toolbelt (prek/gitleaks/
    # cargo-deny/…), wired into the devShell so the discipline is the same
    # everywhere. Follows our nixpkgs to keep a single package set.
    guardrails.url = "github:gerchowl/guardrails";
    guardrails.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs =
    { self, nixpkgs, guardrails }:
    let
      lib = nixpkgs.lib;
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forAllSystems = lib.genAttrs systems;
      pkgsFor = system: import nixpkgs { inherit system; };
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
          herdr = pkgs.callPackage ./nix/package.nix { };
        in
        {
          inherit herdr;
          default = herdr;
        }
      );

      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/herdr";
          meta.description = "Run Herdr";
        };
      });

      checks = forAllSystems (system: {
        herdr = self.packages.${system}.default;
        default = self.checks.${system}.herdr;
      });

      devShells = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
        in
        {
          # guardrails brings the governance toolbelt (prek/gitleaks/cargo-deny/
          # …) and auto-installs the pre-commit hooks; `extra` carries herdr's
          # own build toolchain. SDKROOT comes from the darwin stdenv for free,
          # so the only env we restore is the libghostty-vt build tuning.
          default = guardrails.lib.${system}.mkDevShell {
            inherit pkgs;
            extra = with pkgs; [
              cargo
              cargo-nextest
              clippy
              cmake
              just
              ninja
              pkg-config
              rustc
              rustfmt
              zig_0_15
            ];
            hook = ''
              export LIBGHOSTTY_VT_OPTIMIZE=Debug
              export LIBGHOSTTY_VT_SIMD=true
            '';
          };
        }
      );

      formatter = forAllSystems (system: (pkgsFor system).nixfmt);

      overlays.default = final: _prev: {
        herdr = final.callPackage ./nix/package.nix { };
      };
    };
}
