{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.programs.herdr;
  tomlFormat = pkgs.formats.toml { };

  # Fetch the latest release binary from GitHub.
  # Automatically picks the right platform.
  # Update version + hashes when a new release is published:
  #   nix-prefetch-url https://github.com/ogulcancelik/herdr/releases/download/v<VERSION>/herdr-linux-x86_64
  herdr-version = "0.6.1";
  herdr-release =
    let
      arch = if pkgs.stdenv.hostPlatform.isAarch64 then "aarch64" else "x86_64";
      os =
        if pkgs.stdenv.hostPlatform.isDarwin then
          "macos"
        else
          "linux";
      hashes = {
        x86_64-linux = "sha256-gatwYkmHXbNFcp32Q31yYV0PaQnoNYw2gDvPPfE6up4=";
        aarch64-linux = "sha256-COQTNDaBcK2oXQYwD3FJgx/7VsCPB9z0nII/vyKsAKM=";
        x86_64-darwin = "sha256-ZVf4+i6En8onpzn9r4FpchjhClD3Ew2c1L/lI8LxiK4=";
        aarch64-darwin = "sha256-GG+JoD3rgBHhaYEUcQ+0kXlWK5h21S/q7Fysu3Zd0co=";
      };
      system = pkgs.stdenv.hostPlatform.system;
    in
    pkgs.stdenv.mkDerivation {
      pname = "herdr";
      version = herdr-version;
      src = pkgs.fetchurl {
        url = "https://github.com/ogulcancelik/herdr/releases/download/v${herdr-version}/herdr-${os}-${arch}";
        hash = hashes.${system} or (throw "herdr: no pre-built binary for ${system}");
      };
      phases = [ "installPhase" ];
      installPhase = ''
        mkdir -p $out/bin
        cp $src $out/bin/herdr
        chmod +x $out/bin/herdr
      '';
    };
in
{
  options.programs.herdr = {
    enable = lib.mkEnableOption "herdr";

    package = lib.mkOption {
      type = lib.types.package;
      default = herdr-release;
      defaultText = lib.literalExpression "herdr from GitHub releases (latest)";
      description = ''
        The herdr package to use. Defaults to the latest GitHub release binary.
        To build from source instead, use the flake overlay:

            programs.herdr.package = inputs.herdr.packages.''${system}.default;
      '';
    };

    settings = lib.mkOption {
      inherit (tomlFormat) type;
      default = { };
      example = {
        theme.name = "catppuccin";
        terminal.default_shell = "${pkgs.zsh}/bin/zsh";
        terminal.new_cwd = "follow";
        ui.sidebar_width = 26;
        ui.mouse_capture = true;
        keys.prefix = "ctrl+b";
        keys.new_tab = "prefix+c";
        keys.new_workspace = "prefix+shift+n";
      };
      description = ''
        Configuration written to
        {file}`$XDG_CONFIG_HOME/herdr/config.toml`.
        See <https://herdr.dev> for all options.
      '';
    };

    shellIntegration = {
      enable = lib.mkEnableOption "herdr shell integration" // {
        default = true;
      };

      bash = {
        enable = lib.mkEnableOption "herdr bash integration" // {
          default = config.programs.bash.enable;
        };
      };

      zsh = {
        enable = lib.mkEnableOption "herdr zsh integration" // {
          default = config.programs.zsh.enable;
        };
      };

      fish = {
        enable = lib.mkEnableOption "herdr fish integration" // {
          default = config.programs.fish.enable;
        };
      };
    };
  };

  config = lib.mkIf cfg.enable {
    home.packages = [ cfg.package ];

    xdg.configFile."herdr/config.toml" = lib.mkIf (cfg.settings != { }) {
      source = tomlFormat.generate "herdr-config" cfg.settings;
    };

    programs.bash.initExtra = lib.mkIf (cfg.shellIntegration.enable && cfg.shellIntegration.bash.enable) ''
      eval "$(herdr integration shell bash 2>/dev/null || true)"
    '';

    programs.zsh.initExtra = lib.mkIf (cfg.shellIntegration.enable && cfg.shellIntegration.zsh.enable) ''
      eval "$(herdr integration shell zsh 2>/dev/null || true)"
    '';

    programs.fish.shellInit = lib.mkIf (cfg.shellIntegration.enable && cfg.shellIntegration.fish.enable) ''
      herdr integration shell fish 2>/dev/null | source
    '';
  };
}
