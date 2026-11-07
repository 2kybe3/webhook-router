use std::{collections::HashMap, fs, path::Path};

use anyhow::{Context, anyhow, bail};
use mlua::LuaSerdeExt;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    pub ip: String,
    pub port: u16,

    pub webhooks: HashMap<String, Webhook>,
    pub inputs: HashMap<String, Input>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Input {
    /// A list of rules which decide where to forward/clone the webhook too
    pub rules: Vec<Rule>,
    #[serde(skip)]
    pub token: Option<String>,
    /// The file containing the token
    pub token_file: String,
    /// The default webhook to use
    pub fallback_target: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Webhook {
    /// The url of the webhook
    pub url: Option<String>,
    /// A file containing the url of the webhook
    /// Takes priority over `url`
    pub url_file: Option<String>,
    /// A lua function that formats the webhook
    pub formatter: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Rule {
    /// The name of the rule
    pub name: String,
    /// The lua function of the rule
    pub script: String,
}

#[derive(Debug, Clone)]
pub enum RuleResult {
    /// The request processor should stop execution
    Block,
    /// The request processor should continue execution
    Continue,
    /// The request processor should stop execution and send the webhook to the listed webhooks
    Redirect(Vec<String>),
    /// The request processor should continue execution and send the webhook to the listed webhooks
    Clone(Vec<String>),
}

impl Config {
    pub fn from_file(path: impl AsRef<Path>, only_validate: bool) -> anyhow::Result<Self> {
        Self::from_str(
            &fs::read_to_string(&path)
                .context(format!("failed read config file: {:?}", path.as_ref()))?,
            only_validate,
        )
    }

    pub fn from_str(content: &str, only_validate: bool) -> anyhow::Result<Self> {
        let mut config: Config = toml::from_str(content).context("failed to parse config")?;

        config.validate()?;

        if !only_validate {
            config.init()?;
        }

        Ok(config)
    }

    pub fn init(&mut self) -> anyhow::Result<()> {
        for (name, input) in self.inputs.iter_mut() {
            input.token = Some(
                fs::read_to_string(&input.token_file)
                    .context(format!("could not read token_file for '{name}'"))?
                    .trim()
                    .to_string(),
            );
        }

        for (name, webhook) in self.webhooks.iter_mut() {
            let Some(url_file) = &webhook.url_file else {
                continue;
            };

            webhook.url = Some(
                fs::read_to_string(url_file)
                    .context(format!("could not read url_file for '{name}'"))?
                    .trim()
                    .to_string(),
            );
        }

        Ok(())
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if self.webhooks.is_empty() {
            bail!("at least one webhook must be configured");
        }
        if self.inputs.is_empty() {
            bail!("at least one input must be configured");
        }

        for (name, input) in &self.inputs {
            // Make sure all rules are valid lua
            for rule in &input.rules {
                Self::create_lua_context()
                    .load(&rule.script)
                    .eval::<mlua::Function>()
                    .context(format!("input: '{}'", name))
                    .context(format!("rule: '{}'", rule.name))
                    .context(format!("script: {}", rule.script))?;
            }

            // Make sure the fallback references a valid webhook name
            if let Some(fallback_target) = &input.fallback_target
                && !self
                    .webhooks
                    .iter()
                    .any(|(name, _webhook)| name == fallback_target)
            {
                bail!(
                    "input '{}' references unknown webhook '{}'",
                    name,
                    fallback_target
                );
            }
        }

        // Make sure all webhook formatters are valid lua
        for (name, webhook) in &self.webhooks {
            Self::create_lua_context()
                .load(&webhook.formatter)
                .eval::<mlua::Function>()
                .context(format!("webhook: {name}"))
                .context(format!("script: {}", webhook.formatter))?;
        }

        Ok(())
    }

    fn create_lua_context() -> mlua::Lua {
        mlua::Lua::new()
    }

    pub fn evaluate_rule(&self, rule: &Rule, data: &str) -> anyhow::Result<Option<RuleResult>> {
        let lua = Self::create_lua_context();

        let lua_data = lua
            .to_value(
                &serde_json::from_str::<serde_json::Value>(data)
                    .context("failed to parse input")?,
            )
            .context("failed to convert JSON to Lua")?;

        let function = lua
            .load(&rule.script)
            .eval::<mlua::Function>()
            .context(format!("rule: {}", rule.name))?;

        let result = function
            .call::<mlua::MultiValue>(lua_data)
            .context(format!("error executing rules '{}' script", rule.name))?;

        if result.is_empty() {
            return Ok(None);
        };

        let action = result
            .front()
            .ok_or_else(|| anyhow!("rule script must return an action"))?;

        let action_str = match action {
            mlua::Value::String(s) => s.to_str()?.to_string(),
            mlua::Value::Nil => return Ok(Some(RuleResult::Continue)),
            _ => bail!("rule action must be a string"),
        };

        if result.get(2).is_some() {
            bail!("A 3. argument got returned. (you probably wanted to return a list)")
        }

        match action_str.as_str() {
            "block" | "drop" => Ok(Some(RuleResult::Block)),
            "continue" => Ok(Some(RuleResult::Continue)),
            "redirect" => {
                let targets = result
                    .get(1)
                    .ok_or_else(|| anyhow!("redirect action requires targets"))?;
                Ok(Some(RuleResult::Redirect(
                    self.lua_value_to_string_vec(targets)?,
                )))
            }
            "clone" => {
                let targets = result
                    .get(1)
                    .ok_or_else(|| anyhow!("clone action requires targets"))?;
                Ok(Some(RuleResult::Clone(
                    self.lua_value_to_string_vec(targets)?,
                )))
            }
            _ => bail!("invalid rule action: {}", action_str),
        }
    }

    fn lua_value_to_string_vec(&self, value: &mlua::Value) -> anyhow::Result<Vec<String>> {
        match value {
            mlua::Value::String(s) => Ok(vec![s.to_str()?.to_string()]),
            mlua::Value::Table(table) => {
                let mut results = Vec::new();

                for i in 1..=table.raw_len() {
                    let val = table.get::<mlua::Value>(i)?;
                    match val {
                        mlua::Value::String(s) => results.push(s.to_str()?.to_string()),
                        _ => bail!("targets table must contain only strings"),
                    }
                }

                Ok(results)
            }
            _ => bail!("targets must be a string or table of strings"),
        }
    }

    pub fn get_target_webhooks(&self, input_name: &str, data: &str) -> anyhow::Result<Vec<String>> {
        let input = self
            .inputs
            .get(input_name)
            .ok_or_else(|| anyhow!("unknown input '{input_name}'"))?;

        let mut target_webhooks = Vec::new();

        for rule in &input.rules {
            match self.evaluate_rule(rule, data) {
                Ok(Some(RuleResult::Block)) => return Ok(target_webhooks),
                Ok(Some(RuleResult::Redirect(targets))) => {
                    for target in targets {
                        if !target_webhooks.contains(&target) {
                            target_webhooks.push(target);
                        }
                    }
                    return Ok(target_webhooks);
                }
                Ok(Some(RuleResult::Clone(targets))) => {
                    for target in targets {
                        if !target_webhooks.contains(&target) {
                            target_webhooks.push(target);
                        }
                    }
                }
                Ok(Some(RuleResult::Continue)) | Ok(None) => continue,
                Err(e) => {
                    tracing::error!("error evaluating rule '{}': {}", rule.name, e);
                    continue;
                }
            }
        }

        if target_webhooks.is_empty()
            && let Some(default_target) = &input.fallback_target
        {
            target_webhooks.push(default_target.clone());
        }

        Ok(target_webhooks)
    }

    pub fn format_webhook_body(
        &self,
        webhook: (&String, &Webhook),
        data: &str,
    ) -> anyhow::Result<serde_json::Value> {
        let lua = Self::create_lua_context();

        let json: serde_json::Value =
            serde_json::from_str(data).context("failed to parse input")?;

        let data_table = lua.to_value(&json)?;

        let function = lua
            .load(&webhook.1.formatter)
            .eval::<mlua::Function>()
            .context(format!("webhook: {}", webhook.0))?;

        let result = function
            .call::<mlua::Value>(data_table)
            .context(format!("webhook: {}", webhook.0))?;

        Ok(lua.from_value::<serde_json::Value>(result)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_webhook(name: &str) -> Webhook {
        Webhook {
            url: Some(format!("https://{name}.example.come")),
            url_file: None,
            formatter: r#"function(data) return { body = "test" end}"#.to_string(),
        }
    }

    fn test_webhook_with_formatter(name: &str, formatter: impl Into<String>) -> Webhook {
        Webhook {
            url: Some(format!("https://{name}.example.come")),
            url_file: None,
            formatter: formatter.into(),
        }
    }

    #[test]
    fn test_block_rule() -> anyhow::Result<()> {
        let mut webhooks = HashMap::new();
        webhooks.insert("a".to_string(), test_webhook("a"));
        webhooks.insert("b".to_string(), test_webhook("b"));
        let config = Config {
            ip: "0.0.0.0".to_string(),
            port: 3000,
            webhooks,
            inputs: {
                let mut map = HashMap::new();
                map.insert(
                    "input".to_string(),
                    Input {
                        fallback_target: Some("webhook_a".to_string()),
                        token: None,
                        token_file: "".to_string(),
                        rules: vec![Rule {
                            name: "block_high_priority".to_string(),
                            script: r#"
                                function(data)
                                    if data.priority == "high" then
                                        return "block"
                                    end
                                end
                            "#
                            .to_string(),
                        }],
                    },
                );
                map
            },
        };

        let json = r#"{"type": "alert", "priority": "high"}"#;
        let result = config.get_target_webhooks("input", json)?;
        assert_eq!(result, Vec::<String>::new());
        Ok(())
    }

    #[test]
    fn test_redirect_rule() -> anyhow::Result<()> {
        let mut webhooks = HashMap::new();
        webhooks.insert("a".to_string(), test_webhook("a"));
        webhooks.insert("b".to_string(), test_webhook("b"));
        let config = Config {
            ip: "0.0.0.0".to_string(),
            port: 3000,
            webhooks,
            inputs: {
                let mut map = HashMap::new();
                map.insert(
                    "input".to_string(),
                    Input {
                        fallback_target: Some("webhook_a".to_string()),
                        token: None,
                        token_file: "".to_string(),
                        rules: vec![Rule {
                            name: "redirect_rule".to_string(),
                            script: r#"
                                function(data)
                                    if data.type == "redirect_me" then
                                        return "redirect", "webhook_b"
                                    end
                                end
                            "#
                            .to_string(),
                        }],
                    },
                );
                map
            },
        };

        let json = r#"{"type": "redirect_me"}"#;
        let result = config.get_target_webhooks("input", json)?;
        assert_eq!(result, vec!["webhook_b".to_string()]);
        Ok(())
    }

    #[test]
    fn test_clone_rule() -> anyhow::Result<()> {
        let mut webhooks = HashMap::new();
        webhooks.insert("a".to_string(), test_webhook("a"));
        webhooks.insert("b".to_string(), test_webhook("b"));
        let config = Config {
            ip: "0.0.0.0".to_string(),
            port: 3000,
            webhooks,
            inputs: {
                let mut map = HashMap::new();
                map.insert(
                    "input".to_string(),
                    Input {
                        fallback_target: Some("webhook_a".to_string()),
                        token: None,
                        token_file: "".to_string(),
                        rules: vec![Rule {
                            name: "clone_rule".to_string(),
                            script: r#"
                                function(data)
                                    if data.clone_me == "true" then
                                        return "clone", {"webhook_b"}
                                    end
                                end
                            "#
                            .to_string(),
                        }],
                    },
                );
                map
            },
        };

        let json = r#"{"clone_me": "true", "message": "hello"}"#;
        let result = config.get_target_webhooks("input", json)?;
        assert_eq!(result.len(), 1);
        assert!(result.contains(&"webhook_b".to_string()));
        Ok(())
    }

    #[test]
    fn test_formatter_script() -> anyhow::Result<()> {
        let mut webhooks = HashMap::new();
        webhooks.insert(
            "a".to_string(),
            test_webhook_with_formatter(
                "a",
                r#"
                    function(data)
                        return {
                            body = data.user.name .. " created " .. data.action.type,
                            msgtype = "m.text"
                        }
                    end
                "#,
            ),
        );
        let config = Config {
            ip: "0.0.0.0".to_string(),
            port: 3000,
            webhooks,
            inputs: HashMap::new(),
        };

        let json = r#"{"user": {"name": "alice"}, "action": {"type": "issue"}}"#;
        let result = config.format_webhook_body(config.webhooks.iter().next().unwrap(), json)?;

        assert_eq!(
            result.get("body").and_then(|v| v.as_str()),
            Some("alice created issue")
        );
        assert_eq!(
            result.get("msgtype").and_then(|v| v.as_str()),
            Some("m.text")
        );
        Ok(())
    }

    #[test]
    fn test_rule_sequence_stops_on_redirect() -> anyhow::Result<()> {
        let mut webhooks = HashMap::new();
        webhooks.insert("a".to_string(), test_webhook("a"));
        webhooks.insert("b".to_string(), test_webhook("b"));
        webhooks.insert("c".to_string(), test_webhook("c"));
        let config = Config {
            ip: "0.0.0.0".to_string(),
            port: 3000,
            webhooks,
            inputs: {
                let mut map = HashMap::new();
                map.insert(
                    "input".to_string(),
                    Input {
                        fallback_target: Some("webhook_a".to_string()),
                        token: None,
                        token_file: "".to_string(),
                        rules: vec![
                            Rule {
                                name: "redirect_rule".to_string(),
                                script: r#"
                                    function(data)
                                        if data.type == "redirect_me" then
                                            return "redirect", "webhook_b"
                                        end
                                    end
                                "#
                                .to_string(),
                            },
                            Rule {
                                name: "clone_rule".to_string(),
                                script: r#"
                                    function(data)
                                        return "clone", {"webhook_c"}
                                    end
                                "#
                                .to_string(),
                            },
                        ],
                    },
                );
                map
            },
        };

        let json = r#"{"type": "redirect_me", "other": "something"}"#;
        let result = config.get_target_webhooks("input", json)?;
        assert_eq!(result, vec!["webhook_b".to_string()]);
        Ok(())
    }
}
