use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    str::FromStr,
};

use anyhow::{Context, anyhow, bail};
use mlua::prelude::*;
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
    pub rules: Vec<Rule>,
    #[serde(skip)]
    pub token: Option<String>,
    pub token_file: String,
    pub fallback_target: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Webhook {
    pub url: Option<String>,
    pub url_file: Option<String>,
    pub formatter: WebhookFormatter,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Rule {
    pub name: String,
    pub script: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WebhookFormatter {
    pub script: String,
}

#[derive(Debug, Clone)]
pub enum RuleResult {
    Block,
    Continue,
    Redirect(Vec<String>),
    Clone(Vec<String>),
}

impl Config {
    pub fn from_file(path: impl AsRef<Path>, validate: bool) -> anyhow::Result<Self> {
        let content = fs::read_to_string(&path)
            .context(format!("failed to read config file: {:?}", path.as_ref()))?;

        Self::from_str(&content, validate)
    }

    pub fn from_str(content: &str, validate: bool) -> anyhow::Result<Self> {
        let mut config: Config =
            toml::from_str(content).context("failed to parse config as TOML")?;

        config.validate()?;

        if !validate {
            config.init()?;
        }

        Ok(config)
    }

    pub fn init(&mut self) -> anyhow::Result<()> {
        for input in &mut self.inputs {
            tracing::info!("getting token for input '{}'", input.0);
            let token = fs::read_to_string(PathBuf::from_str(&input.1.token_file)?)?;
            input.1.token = Some(token.trim().to_string());
        }
        for webhook in self.webhooks.iter_mut() {
            if let Some(url_file) = &webhook.1.url_file {
                tracing::info!("getting url for input '{}'", webhook.0);
                let url = fs::read_to_string(PathBuf::from_str(url_file)?)?;
                webhook.1.url = Some(url.trim().to_string());
            }
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

        let webhook_names: Vec<_> = self.webhooks.iter().map(|w| w.0).collect();
        if webhook_names.len() != self.webhooks.iter().len() {
            bail!("webhooks names must be unique");
        }

        for (input_name, input) in &self.inputs {
            for rule in &input.rules {
                let lua = Self::create_lua_context();
                lua.load(&rule.script).eval::<LuaFunction>().map_err(|e| {
                    anyhow!(
                        "invalid Lua script.\nrule: '{}'\nerror '{}'\nscript:\n```\n{}\n```\n",
                        rule.name,
                        e,
                        rule.script
                    )
                })?;
            }

            if let Some(target) = &input.fallback_target
                && !self.webhooks.iter().any(|w| w.0 == target)
            {
                bail!(
                    "input '{}' references unknown webhook '{}'",
                    input_name,
                    target
                );
            }
        }

        for webhook in &self.webhooks {
            let lua = Self::create_lua_context();
            lua.load(&webhook.1.formatter.script).eval::<LuaFunction>().map_err(|e| {
                    anyhow!(
                        "invalid Lua script in webhook formatter '{}'\nerror: {}\nscript:\n```\n{}\n```",
                        webhook.0,
                        e,
                        webhook.1.formatter.script
                    )
                })?;
        }

        Ok(())
    }

    fn create_lua_context() -> Lua {
        Lua::new()
    }

    pub fn evaluate_rule(&self, rule: &Rule, data: &str) -> anyhow::Result<Option<RuleResult>> {
        let lua = Self::create_lua_context();

        let json: serde_json::Value =
            serde_json::from_str(data).context("failed to parse input as JSON")?;
        let data_table = lua
            .to_value(&json)
            .map_err(|e| anyhow!("{e:?}"))
            .context("failed to convert JSON to Lua")?;

        let function = lua
            .load(&rule.script)
            .eval::<LuaFunction>()
            .map_err(|e| anyhow!("{e}"))
            .context(format!("failed to compile rule '{}' script", rule.name))?;

        let result = function
            .call::<mlua::MultiValue>(data_table)
            .map_err(|e| anyhow!("{e}"))
            .context(format!("error executing rule '{}' script", rule.name))?;

        if result.is_empty() {
            return Ok(None);
        };

        let action = result
            .front()
            .ok_or_else(|| anyhow!("rule script must return an action"))?;

        let action_str = match action {
            LuaValue::String(s) => s.to_str().map_err(|e| anyhow!("{e}"))?.to_string(),
            LuaValue::Nil => "continue".to_string(),
            _ => bail!("rule action must be a string"),
        };

        match action_str.as_str() {
            "block" => Ok(Some(RuleResult::Block)),
            "continue" => Ok(Some(RuleResult::Continue)),
            "redirect" => {
                let targets = result
                    .get(1)
                    .ok_or_else(|| anyhow!("redirect action requires targets"))?;
                let targets_vec = self.lua_value_to_string_vec(targets)?;
                Ok(Some(RuleResult::Redirect(targets_vec)))
            }
            "clone" => {
                let targets = result
                    .get(1)
                    .ok_or_else(|| anyhow!("clone action requires targets"))?;
                let targets_vec = self.lua_value_to_string_vec(targets)?;
                Ok(Some(RuleResult::Clone(targets_vec)))
            }
            _ => bail!("invalid rule action: {}", action_str),
        }
    }

    fn lua_value_to_string_vec(&self, value: &LuaValue) -> anyhow::Result<Vec<String>> {
        match value {
            LuaValue::String(s) => Ok(vec![s.to_str().map_err(|e| anyhow!("{e}"))?.to_string()]),
            LuaValue::Table(t) => {
                let mut results = Vec::new();
                for i in 1..=t.raw_len() {
                    let val = t.get::<LuaValue>(i).map_err(|e| anyhow!("{e}"))?;
                    match val {
                        LuaValue::String(s) => {
                            results.push(s.to_str().map_err(|e| anyhow!("{e}"))?.to_string())
                        }
                        _ => bail!("target table must contain only strings"),
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
            serde_json::from_str(data).context("failed to parse input as JSON")?;
        let data_table = lua.to_value(&json).map_err(|e| anyhow!("{e:?}"))?;

        let function = lua
            .load(&webhook.1.formatter.script)
            .eval::<LuaFunction>()
            .map_err(|e| {
                anyhow!(
                    "failed to compile formatter '{}' script with error '{e}'",
                    webhook.0
                )
            })?;

        let result = function.call::<LuaValue>(data_table).map_err(|e| {
            anyhow!(
                "error executing formatter '{}' script with error '{e}'",
                webhook.0
            )
        })?;

        lua.from_value::<serde_json::Value>(result)
            .map_err(|e| anyhow!("{e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_webhook(name: &str) -> Webhook {
        Webhook {
            url: Some(format!("https://{name}.example.come")),
            url_file: None,
            formatter: WebhookFormatter {
                script: r#"function(data) return { body = "test" end}"#.to_string(),
            },
        }
    }

    fn test_webhook_with_formatter(name: &str, formatter: &str) -> Webhook {
        Webhook {
            url: Some(format!("https://{name}.example.come")),
            url_file: None,
            formatter: WebhookFormatter {
                script: formatter.to_string(),
            },
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
