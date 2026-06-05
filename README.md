# webhook-router

A lightweight, programmable webhook router written in Rust. Route, filter, transform, and fan-out incoming webhooks using **Lua scripts**.

---

## Features

- **Rule-based routing** using Lua scripts (`block`, `redirect`, `continue`, `clone`)
- **Payload transformation** per webhook using Lua formatters
- **Token-based authentication** per input
- **TOML configuration** with validation
- **Detailed response** with per-webhook delivery status

---

## Installation

### From Source

```bash
git clone <your-repo>
cd webhook-router
cargo build --release
```

The binary will be available at `target/release/webhook-router`.

---

## Quick Start

1. Create a configuration file:

```bash
touch config.toml
```

2. Edit `config.toml` (see [Configuration](#configuration) below)

3. Run the server:

```bash
./target/release/webhook-router --config config.toml
```

The server will listen on `http://0.0.0.0:3000`.

---

## Configuration

### Example `config.toml`

```toml
ip = "127.0.0.1"
port = 3000

[webhooks.discord]
url = "https://discord.com/api/webhooks/..."
formatter.script = '''
function(data)
    return {
        content = "**" .. data.repository.name .. "** " .. data.action,
        embeds = {{
            title = data.sender.login,
            url = data.sender.html_url
        }}
    }
end
'''

[webhooks.slack]
url = "https://hooks.slack.com/services/..."
formatter.script = '''
function(data)
    return {
        text = "New event: " .. data.action,
        username = "Webhook Router"
    }
end
'''

[inputs.github]
token_file = "/run/secrets/github-webhook-token"
fallback_target = "discord"

[[inputs.github.rules]]
name = "high_priority_issues"
script = '''
function(data)
    if data.action == "opened" and data.issue and data.issue.labels then
        for _, label in ipairs(data.issue.labels) do
            if label.name == "critical" then
                return "redirect", "slack"
            end
        end
    end
end
'''

[[inputs.github.rules]]
name = "clone_to_both"
script = '''
function(data)
    if data.action == "push" then
        return "clone", {"discord", "slack"}
    end
end
'''
```

### Example Uses

[nix-main](https://git.kybe.xyz/2kybe3/infra-nix-main/src/branch/main/modules/webhook-router.nix) uses webhook-router to filter out renovate issue edit's and redirect renovate embeds into a different discord channel

### Config Structure

- `webhooks`: List of destination webhooks with name, URL, and formatter
- `inputs`: Map of entry points (e.g. `github`, `stripe`, `internal`)
  - Each input has its own `token_file`, optional `fallback_target`, and ordered list of rules

---

## Lua Scripting

### Rule Scripts

Rules must return one of:

- `"block"` → stop and return 200 (no delivery)
- `"continue"` → continue to next rule
- `"redirect", "webhook_name"` → deliver **only** to this webhook
- `"redirect", {"webhook_a", "webhook_b"}` → deliver only to these
- `"clone", "webhook_name"` or `"clone", {"webhook_a", "webhook_b"}` → deliver to these + continue rules

**Example:**

```lua
function(data)
    if data.priority == "high" then
        return "redirect", "urgent-slack"
    elseif data.event == "push" then
        return "clone", {"discord", "github-archive"}
    end
    -- implicit: continue
end
```

### Formatter Scripts

Must return a Lua table that becomes the JSON body sent to the webhook.

```lua
function(data)
    return {
        content = "Event: " .. (data.action or "unknown"),
        username = "MyBot",
        avatar_url = "https://..."
    }
end
```

---

## API Usage

### Sending a Webhook

```http
POST /webhook?input=github&token=your-secret-token
Content-Type: application/json

{ ... your webhook payload ... }
```

### Response

```json
{
  "success": true,
  "message": "webhooks sent",
  "targets": ["discord", "slack"],
  "sent": [
    {
      "webhook": "discord",
      "status": 204,
      "success": true
    },
    {
      "webhook": "slack",
      "status": 200,
      "success": true
    }
  ]
}
```

---

## License

[GNU General Public License v3.0 (GPL-3.0)](./LICENSE.md)
