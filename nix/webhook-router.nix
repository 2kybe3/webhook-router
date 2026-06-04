{
  lib,
  pkgs,
  crane,
  ...
}:
let
  craneLib = (crane.mkLib pkgs).overrideToolchain (p: p.rust-bin.stable.latest.default);

  root = ../.;

  src = lib.fileset.toSource {
    inherit root;
    fileset = lib.fileset.unions [ (craneLib.fileset.commonCargoSources root) ];
  };

  commonArgs = {
    inherit src;
    strictDeps = true;
  };

  cargoArtifacts = craneLib.buildDepsOnly commonArgs;

  webhook-router = craneLib.buildPackage (
    commonArgs
    // {
      inherit cargoArtifacts;
      meta.mainProgram = "webhook-router";
    }
  );

  checks = {
    inherit webhook-router;
    webhook-router-clippy = craneLib.cargoClippy (
      commonArgs
      // {
        inherit cargoArtifacts;
        cargoClippyExtraArgs = "--all-targets -- --deny warnings";
      }
    );
  };

  devShell = craneLib.devShell {
    checks = checks;
  };
in
{
  inherit checks devShell webhook-router;
}
