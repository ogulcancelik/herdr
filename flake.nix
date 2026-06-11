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
          herdr = pkgs.callPackage ./nix/package.nix {
            buildChannel = "fork";
            buildId = self.shortRev or self.dirtyShortRev or null;
          };
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
              sccache
              zig_0_15
            ];
            hook = ''
              export LIBGHOSTTY_VT_OPTIMIZE=Debug
              export LIBGHOSTTY_VT_SIMD=true
              # Shared compile cache across ALL worktrees: each keeps its own
              # target/ (parallel builds stay parallel — no cargo build-dir
              # lock contention), but every rustc invocation hits one cache,
              # so a fresh worktree's first build drops from minutes to ~link
              # time. Opt out per-shell with RUSTC_WRAPPER="".
              export RUSTC_WRAPPER=''${RUSTC_WRAPPER-sccache}
              export SCCACHE_DIR=''${SCCACHE_DIR:-$HOME/.cache/herdr-sccache}
              export SCCACHE_CACHE_SIZE=''${SCCACHE_CACHE_SIZE:-20G}
            '';
          };
        }
      );

      formatter = forAllSystems (system: (pkgsFor system).nixfmt);

      overlays.default = final: _prev: {
        herdr = final.callPackage ./nix/package.nix {
          buildChannel = "fork";
          buildId = self.shortRev or self.dirtyShortRev or null;
        };
      };
    };
}
