{ inputs, ... }:
{
  perSystem =
    { system, ... }:
    let
      pkgs = inputs.nixpkgs.legacyPackages.${system};
      rustBin = inputs.rust-overlay.lib.mkRustBin { } pkgs;
      rustToolchain = rustBin.stable.latest.default;
      craneLib = inputs.crane.mkLib pkgs;
      herdr-pkg = pkgs.callPackage ../package.nix {
        crane = craneLib;
        inherit rustToolchain;
      };
    in
    {
      packages = {
        default = herdr-pkg;
      };
    };
}
