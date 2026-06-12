{
  lib,
  stdenv,
  rustPlatform,
  callPackage,
  runCommand,
  writeShellScriptBin,
  zig_0_15,
  zstd,
  pkg-config,
  git,
  apple-sdk ? null,
  cctools ? null,
  # Build identity baked into `herdr status` / federation summaries:
  # version renders as "<base>-<channel>.<id>" (src/build_info.rs). The
  # flake passes the fork channel + short rev so every deployed build
  # self-identifies — two fork builds are otherwise indistinguishable
  # ("0.6.8" / proto N). null keeps the plain upstream version string.
  buildChannel ? null,
  buildId ? null,
}:

let
  manifest = lib.importTOML ../Cargo.toml;
  zigDeps = callPackage ../vendor/libghostty-vt/build.zig.zon.nix {
    name = "herdr-libghostty-vt-zig-cache";
    inherit zstd;
    linkFarm =
      name: entries:
      runCommand name { } ''
        mkdir -p $out
        ${lib.concatMapStringsSep "\n" (entry: ''
          cp -rL ${entry.path} $out/${entry.name}
        '') entries}
      '';
  };

  darwinSdkRoot = "${apple-sdk}/Platforms/MacOSX.platform/Developer/SDKs/MacOSX.sdk";
  darwinDeveloperDir = "${apple-sdk}/Platforms/MacOSX.platform/Developer";
  darwinXcodeSelect = writeShellScriptBin "xcode-select" ''
    if [ "$1" = "--print-path" ]; then
      echo ${lib.escapeShellArg darwinDeveloperDir}
      exit 0
    fi
    echo "unsupported xcode-select invocation: $*" >&2
    exit 1
  '';
  darwinXcrun = writeShellScriptBin "xcrun" ''
    if [ "$1" = "--sdk" ] && [ "$3" = "--show-sdk-path" ]; then
      echo ${lib.escapeShellArg darwinSdkRoot}
      exit 0
    fi
    echo "unsupported xcrun invocation: $*" >&2
    exit 1
  '';
in
rustPlatform.buildRustPackage {
  pname = "herdr";
  version = manifest.package.version;

  src = lib.fileset.toSource {
    root = ./..;
    fileset = lib.fileset.intersection (lib.fileset.fromSource (lib.sources.cleanSource ./..)) (
      lib.fileset.unions [
        ../assets
        ../src
        ../vendor/libghostty-vt
        ../vendor/libghostty-vt.vendor.json
        ../build.rs
        ../Cargo.lock
        ../Cargo.toml
      ]
    );
  };

  cargoLock = {
    lockFile = ../Cargo.lock;
  };

  nativeBuildInputs = [
    git
    pkg-config
  ]
  ++ lib.optionals stdenv.hostPlatform.isDarwin [
    cctools
    darwinXcodeSelect
    darwinXcrun
  ];

  env = {
    LIBGHOSTTY_VT_OPTIMIZE = "ReleaseFast";
    LIBGHOSTTY_VT_SIMD = "true";
    LIBGHOSTTY_VT_ZIG_SYSTEM_DIR = zigDeps;
    ZIG = lib.getExe zig_0_15;
  }
  // lib.optionalAttrs (buildChannel != null) {
    HERDR_BUILD_CHANNEL = buildChannel;
  }
  // lib.optionalAttrs (buildId != null) {
    HERDR_BUILD_ID = buildId;
  }
  // lib.optionalAttrs stdenv.hostPlatform.isDarwin {
    SDKROOT = darwinSdkRoot;
  };

  preBuild = ''
    export ZIG_GLOBAL_CACHE_DIR="$TMPDIR/zig-global-cache"
    export ZIG_LOCAL_CACHE_DIR="$TMPDIR/zig-local-cache"
  '';

  # Rust tests are covered by the normal CI workflow. The Nix check is
  # intentionally build-only so it validates packaging inputs without
  # duplicating the full Rust test suite.
  doCheck = false;

  meta = {
    description = "Terminal workspace manager for AI coding agents";
    homepage = "https://herdr.dev";
    license = lib.licenses.agpl3Plus;
    mainProgram = "herdr";
    platforms = lib.platforms.linux ++ lib.platforms.darwin;
  };
}
