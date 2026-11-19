use std::path::{Path, PathBuf};

use serde_json::Value;

use super::AdapterId;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelMirrorInfo {
    pub model: String,
    pub provider: Option<String>,
    pub reasoning: Option<String>,
    pub is_manual_override: bool,
}

impl ModelMirrorInfo {
    pub fn display_badge(&self) -> String {
        let mut parts = Vec::new();
        if let Some(p) = &self.provider {
            parts.push(format!("{p}/{}", self.model));
        } else {
            parts.push(self.model.clone());
        }
        if let Some(r) = &self.reasoning {
            parts.push(format!("🧠 {r}"));
        }
        if self.is_manual_override {
            parts.push("(override)".to_string());
        }
        parts.join(" · ")
    }

    pub fn short_badge(&self) -> String {
        let base = if let Some(p) = &self.provider {
            format!("{p}/{}", self.model)
        } else {
            self.model.clone()
        };
        if let Some(r) = &self.reasoning {
            format!("{base} [🧠 {r}]")
        } else {
            base
        }
    }
}

pub fn detect_model_info(adapter: AdapterId, manual_override: Option<&str>) -> ModelMirrorInfo {
    if let Some(model) = manual_override
        && !model.trim().is_empty()
    {
        return ModelMirrorInfo {
            model: model.trim().to_string(),
            provider: None,
            reasoning: None,
            is_manual_override: true,
        };
    }

    let home = std::env::var_os("HOME").map(PathBuf::from);

    match adapter {
        AdapterId::Pi => detect_pi(home.as_deref()),
        AdapterId::Codex => detect_codex(home.as_deref()),
        AdapterId::Claude => detect_claude(home.as_deref()),
        AdapterId::Omp => detect_omp(home.as_deref()),
        AdapterId::OpenCode => detect_opencode(home.as_deref()),
        AdapterId::Kilo => detect_kilo(home.as_deref()),
        AdapterId::Agy => detect_agy(home.as_deref()),
    }
}

fn read_json_file(path: &Path) -> Option<Value> {
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

fn detect_pi(home: Option<&Path>) -> ModelMirrorInfo {
    if let Some(home) = home {
        let candidates = [
            home.join(".pi/agent/settings.json"),
            home.join(".config/pi/settings.json"),
            home.join(".pi/config.json"),
        ];
        for path in &candidates {
            if let Some(json) = read_json_file(path) {
                let model = json
                    .get("defaultModel")
                    .or_else(|| json.get("model"))
                    .and_then(Value::as_str);
                let provider = json
                    .get("defaultProvider")
                    .or_else(|| json.get("provider"))
                    .and_then(Value::as_str);
                let reasoning = json
                    .get("reasoningEffort")
                    .or_else(|| json.get("thinking"))
                    .and_then(|v| match v {
                        Value::String(s) => Some(s.clone()),
                        Value::Bool(true) => Some("on".to_string()),
                        Value::Number(n) => Some(format!("{n}")),
                        _ => None,
                    });

                if let Some(model) = model {
                    return ModelMirrorInfo {
                        model: model.to_string(),
                        provider: provider.map(str::to_string),
                        reasoning,
                        is_manual_override: false,
                    };
                }
            }
        }
    }

    ModelMirrorInfo {
        model: "default".to_string(),
        provider: Some("pi".to_string()),
        reasoning: None,
        is_manual_override: false,
    }
}

fn detect_codex(home: Option<&Path>) -> ModelMirrorInfo {
    if let Some(home) = home {
        let candidates = [
            home.join(".codex/config.json"),
            home.join(".config/codex/config.json"),
        ];
        for path in &candidates {
            if let Some(json) = read_json_file(path) {
                let model = json.get("model").and_then(Value::as_str);
                let reasoning = json
                    .get("reasoning_effort")
                    .or_else(|| json.get("reasoningEffort"))
                    .and_then(Value::as_str)
                    .map(str::to_string);

                if let Some(model) = model {
                    return ModelMirrorInfo {
                        model: model.to_string(),
                        provider: None,
                        reasoning,
                        is_manual_override: false,
                    };
                }
            }
        }
    }

    ModelMirrorInfo {
        model: "o3-mini".to_string(),
        provider: None,
        reasoning: Some("medium".to_string()),
        is_manual_override: false,
    }
}

fn detect_claude(home: Option<&Path>) -> ModelMirrorInfo {
    if let Some(home) = home {
        let candidates = [
            home.join(".claude.json"),
            home.join(".claude/config.json"),
            home.join(".config/claude/config.json"),
        ];
        for path in &candidates {
            if let Some(json) = read_json_file(path) {
                let model = json.get("model").and_then(Value::as_str);
                let reasoning = if json
                    .get("alwaysThinking")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    if let Some(tokens) = json.get("maxThinkingTokens").and_then(Value::as_u64) {
                        Some(format!("{}k", tokens / 1024))
                    } else {
                        Some("on".to_string())
                    }
                } else {
                    None
                };

                if let Some(model) = model {
                    return ModelMirrorInfo {
                        model: model.to_string(),
                        provider: None,
                        reasoning,
                        is_manual_override: false,
                    };
                }
            }
        }
    }

    ModelMirrorInfo {
        model: "claude-3.7-sonnet".to_string(),
        provider: None,
        reasoning: Some("thinking".to_string()),
        is_manual_override: false,
    }
}

fn detect_omp(home: Option<&Path>) -> ModelMirrorInfo {
    if let Some(home) = home {
        let candidates = [
            home.join(".config/omp/config.json"),
            home.join(".omp/config.json"),
        ];
        for path in &candidates {
            if let Some(json) = read_json_file(path) {
                let model = json.get("model").and_then(Value::as_str);
                let provider = json.get("provider").and_then(Value::as_str);
                let reasoning = json.get("thinking").and_then(|v| match v {
                    Value::String(s) => Some(s.clone()),
                    Value::Bool(true) => Some("on".to_string()),
                    _ => None,
                });

                if let Some(model) = model {
                    return ModelMirrorInfo {
                        model: model.to_string(),
                        provider: provider.map(str::to_string),
                        reasoning,
                        is_manual_override: false,
                    };
                }
            }
        }
    }

    ModelMirrorInfo {
        model: "default".to_string(),
        provider: Some("omp".to_string()),
        reasoning: None,
        is_manual_override: false,
    }
}

fn detect_opencode(home: Option<&Path>) -> ModelMirrorInfo {
    if let Some(home) = home {
        let candidates = [
            home.join(".config/opencode/config.json"),
            home.join(".opencode/config.json"),
            PathBuf::from(".opencode.json"),
        ];
        for path in &candidates {
            if let Some(json) = read_json_file(path) {
                let model = json.get("model").and_then(Value::as_str);
                let provider = json.get("provider").and_then(Value::as_str);
                let reasoning = json.get("reasoning").and_then(|v| match v {
                    Value::String(s) => Some(s.clone()),
                    Value::Bool(true) => Some("on".to_string()),
                    _ => None,
                });

                if let Some(model) = model {
                    return ModelMirrorInfo {
                        model: model.to_string(),
                        provider: provider.map(str::to_string),
                        reasoning,
                        is_manual_override: false,
                    };
                }
            }
        }
    }

    ModelMirrorInfo {
        model: "default".to_string(),
        provider: Some("opencode".to_string()),
        reasoning: None,
        is_manual_override: false,
    }
}

fn detect_kilo(home: Option<&Path>) -> ModelMirrorInfo {
    if let Some(home) = home {
        let candidates = [
            home.join(".config/kilo/config.json"),
            home.join(".kilo/config.json"),
        ];
        for path in &candidates {
            if let Some(json) = read_json_file(path) {
                let model = json.get("model").and_then(Value::as_str);
                let provider = json.get("provider").and_then(Value::as_str);
                if let Some(model) = model {
                    return ModelMirrorInfo {
                        model: model.to_string(),
                        provider: provider.map(str::to_string),
                        reasoning: None,
                        is_manual_override: false,
                    };
                }
            }
        }
    }

    ModelMirrorInfo {
        model: "default".to_string(),
        provider: Some("kilo".to_string()),
        reasoning: None,
        is_manual_override: false,
    }
}

fn detect_agy(home: Option<&Path>) -> ModelMirrorInfo {
    if let Some(home) = home {
        let path = home.join(".gemini/antigravity/settings.json");
        if let Some(json) = read_json_file(&path)
            && let Some(model) = json.get("model").and_then(Value::as_str)
        {
            return ModelMirrorInfo {
                model: model.to_string(),
                provider: Some("google".to_string()),
                reasoning: None,
                is_manual_override: false,
            };
        }
    }

    ModelMirrorInfo {
        model: "gemini-2.5-pro".to_string(),
        provider: Some("google".to_string()),
        reasoning: None,
        is_manual_override: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_override_takes_precedence() {
        let info = detect_model_info(AdapterId::Codex, Some("gpt-4o"));
        assert_eq!(info.model, "gpt-4o");
        assert!(info.is_manual_override);
        assert_eq!(info.display_badge(), "gpt-4o · (override)");
    }

    #[test]
    fn fallback_defaults_have_sensible_models() {
        let codex = detect_model_info(AdapterId::Codex, None);
        assert!(!codex.model.is_empty());
        assert!(!codex.is_manual_override);

        let claude = detect_model_info(AdapterId::Claude, None);
        assert!(!claude.model.is_empty());

        let agy = detect_model_info(AdapterId::Agy, None);
        assert_eq!(agy.model, "gemini-2.5-pro");
    }

    #[test]
    fn badge_formatting_includes_provider_and_reasoning() {
        let info = ModelMirrorInfo {
            model: "deepseek-r1".to_string(),
            provider: Some("openrouter".to_string()),
            reasoning: Some("high".to_string()),
            is_manual_override: false,
        };
        assert_eq!(info.display_badge(), "openrouter/deepseek-r1 · 🧠 high");
        assert_eq!(info.short_badge(), "openrouter/deepseek-r1 [🧠 high]");
    }
}
