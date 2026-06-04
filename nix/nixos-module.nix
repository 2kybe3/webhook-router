{
  lib,
  self,
  pkgs,
  config,
  ...
}:
let
  cfg = config.services.webhook-router;
  format = pkgs.formats.toml { };
  conf = format.generate "webhook-router-config.toml" cfg.settings;
in
{
  options.services.webhook-router = {
    enable = lib.mkEnableOption "webhook-router";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      description = "webhook-router package to use.";
    };

    settings = lib.mkOption {
      type = format.type;
      default = {
        ip = "127.0.0.1";
        port = 3000;
      };
      example = {
        ip = "127.0.0.1";
        port = 3000;
      };
      description = "TOML configuration for webhook-router.";
    };

    openFirewall = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = ''
        Whether to open the default ports in the firewall for the webhook-router server.
      '';
    };

    user = lib.mkOption {
      type = lib.types.str;
      default = "webhook-router";
      description = "User under which the service runs.";
    };

    group = lib.mkOption {
      type = lib.types.str;
      default = "webhook-router";
      description = "Group under which the service runs.";
    };

    validateConfig = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Validate the configuration before starting the service.";
    };
  };

  config = lib.mkIf cfg.enable {
    users.users.${cfg.user} = {
      isSystemUser = true;
      group = cfg.group;
      description = "Webhook Router service user";
    };

    users.groups.${cfg.group} = { };

    networking.firewall = lib.mkIf cfg.openFirewall {
      allowedTCPPorts = [
        cfg.settings.port
      ];
    };

    systemd.services.webhook-router = {
      description = "Webhook Router Service";
      wantedBy = [ "multi-user.target" ];
      after = [ "network.target" ];

      serviceConfig =
        let
          validatedConfig =
            file:
            pkgs.runCommand "validate-webhook-router-conf"
              {
                nativeBuildInputs = [ cfg.package ];
              }
              ''
                webhook-router --config ${file} --validate
                ln -s "${file}" "$out"
              '';
        in
        {
          ExecStart = "${lib.getExe cfg.package} --config ${
            if cfg.validateConfig then (validatedConfig conf) else conf
          }";
          User = cfg.user;
          Group = cfg.group;
          Restart = "always";
          RestartSec = 5;
        };
    };
  };
}
