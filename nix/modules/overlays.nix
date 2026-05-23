{ inputs, ... }:
{
  flake.overlays.default = final: prev: {
    herdr = inputs.self.packages.${prev.system}.default;
  };
}
