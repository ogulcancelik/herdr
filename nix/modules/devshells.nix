{ inputs, ... }:
{
  perSystem =
    { system, pkgs, ... }:
    let
      rustBin = inputs.rust-overlay.lib.mkRustBin { } pkgs;
      rustToolchain = rustBin.stable.latest.default;
    in
    {
      devShells.default = pkgs.mkShell {
        name = "herdr-dev";

        packages = with pkgs; [
          rustToolchain
          cargo-nextest
          just
          zig_0_15
          cmake
          ninja
          pkg-config
        ];

        env = {
          LIBGHOSTTY_VT_OPTIMIZE = "Debug";
          LIBGHOSTTY_VT_SIMD = "true";
        };
      };
    };
}
