{
  testers,
  nixosModule,
  ...
}:
testers.nixosTest {
  name = "nixos-module-check";

  nodes.machine =
    { ... }:
    {
      imports = [ nixosModule ];

      services.webhook-router = {
        enable = true;
        openFirewall = true;

        settings = {
          ip = "127.0.0.1";
          port = 3000;

          webhooks.test-webhook = {
            url = "http://127.0.0.1:9999";
            formatter.script = ''
              function(data)
                return {
                  message = data.body or data.message or "No content",
                  status = "processed"
                }
              end
            '';
          };

          inputs.test-input = {
            token_file = builtins.toFile "token.txt" "supersecret123";
            fallback_target = "test-webhook";

            rules = [
              {
                name = "block-high-priority";
                script = ''
                  function(data)
                    if data.priority == "high" then
                      return "block"
                    end
                  end
                '';
              }
              {
                name = "redirect-low-priority";
                script = ''
                  function(data)
                    if data.priority == "low" then
                      return "redirect", "test-webhook"
                    end
                  end
                '';
              }
            ];
          };
        };
      };
    };

  testScript = ''
    machine.wait_for_unit("webhook-router.service")
    machine.wait_for_open_port(3000)

    result = machine.succeed("curl -s -X POST -H 'Content-Type: application/json' 'http://127.0.0.1:3000/webhook?input=test-input&token=supersecret123' -d '{\"priority\": \"high\", \"message\": \"This should be blocked\"}'")
    assert "webhook blocked by rule" in result, f"Expected block, got: {result}"

    result = machine.succeed("curl -s -X POST -H 'Content-Type: application/json' 'http://127.0.0.1:3000/webhook?input=test-input&token=supersecret123' -d '{\"priority\": \"low\", \"message\": \"This should go through\"}'")
    assert '"success":false' in result, f"Expected no success, got: {result}"
    assert "test-webhook" in result, f"Expected test-webhook in targets, got: {result}"

    result = machine.succeed("curl -s -w '%{http_code}' -X POST -H 'Content-Type: application/json' 'http://127.0.0.1:3000/webhook?input=test-input&token=wrongtoken' -d '{\"priority\": \"low\"}' | tail -c 3")
    assert result.strip() == "401", f"Expected 401 Unauthorized, got: {result}"

    result = machine.succeed("curl -s -w '%{http_code}' -X POST 'http://127.0.0.1:3000/webhook?input=unknown&token=supersecret123' -d '{}' | tail -c 3")
    assert result.strip() == "404", f"Expected 404 Not Found, got: {result}"
  '';
}
