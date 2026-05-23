{
  lib,
  stdenv,
  crane,
  rustToolchain,
  optimize ? "release",
}:
let
  craneLib = crane.overrideToolchain rustToolchain;

  src = lib.fileset.toSource {
    root = ./..;
    fileset = lib.fileset.unions [
      ../src
      ../assets
      ../vendor
      ../build.rs
      ../Cargo.toml
      ../Cargo.lock
    ];
  };

  cargoArtifacts = craneLib.buildDepsOnly {
    inherit src;
  };
in
craneLib.buildPackage {
  inherit src cargoArtifacts;

  meta = {
    description = "Terminal workspace manager for AI coding agents";
    homepage = "https://herdr.dev";
    license = lib.licenses.agpl3Only;
    platforms = lib.platforms.linux ++ lib.platforms.darwin;
    mainProgram = "herdr";
  };
}
