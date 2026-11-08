//! UX contracts for team model routing, reports and safe file exports.
use std::collections::{BTreeMap, HashSet};
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use ryoko_core::team::standard_team;
use ryoko_models::{ProviderCatalog, Usage};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TeamSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    #[serde(default)]
    pub role_models: BTreeMap<String, String>,
}

fn bad(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

impl TeamSettings {
    pub fn from_json(text: &str) -> io::Result<Self> {
        let parsed: Self = serde_json::from_str(text)
            .map_err(|err| bad(format!("invalid team settings: {err}")))?;
        parsed.validate()?;
        Ok(parsed)
    }

    pub fn validate(&self) -> io::Result<()> {
        let roles: HashSet<String> = standard_team()
            .iter()
            .map(|member| member.role.label().to_owned())
            .collect();
        if let Some(model) = &self.default_model {
            valid_model(model)?;
        }
        for (role, model) in &self.role_models {
            if !roles.contains(role) {
                return Err(bad(format!(
                    "unknown team role {role}; use ryoko roles for valid names"
                )));
            }
            valid_model(model)?;
        }
        Ok(())
    }

    pub fn model_for(&self, role: &str, cli_default: Option<&str>) -> io::Result<String> {
        let selected = self
            .role_models
            .get(role)
            .map(String::as_str)
            .or(cli_default)
            .or(self.default_model.as_deref())
            .ok_or_else(|| {
                bad(format!(
                    "no model configured for {role}; set --model or default_model"
                ))
            })?;
        valid_model(selected)?;
        Ok(selected.to_owned())
    }

    pub fn check_catalog(
        &self,
        catalog: &ProviderCatalog,
        cli_default: Option<&str>,
    ) -> io::Result<Vec<(String, String)>> {
        self.validate()?;
        let mut assignments = Vec::new();
        for member in standard_team() {
            let model = self.model_for(member.role.label(), cli_default)?;
            let provider = model
                .split_once('/')
                .map_or(catalog.default_provider.as_str(), |(provider, _)| provider);
            if !catalog
                .providers
                .iter()
                .any(|profile| profile.id == provider)
            {
                return Err(bad(format!(
                    "role {} selects unknown provider {provider}",
                    member.role
                )));
            }
            assignments.push((member.role.label().to_owned(), model));
        }
        Ok(assignments)
    }
}

fn valid_model(model: &str) -> io::Result<()> {
    if model.trim().is_empty()
        || model.trim() != model
        || model.chars().any(char::is_control)
        || model
            .split_once('/')
            .is_some_and(|(left, right)| left.is_empty() || right.is_empty())
    {
        return Err(bad(
            "model IDs must be nonempty, trimmed, and use provider/model syntax",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct TeamTurn {
    pub role: String,
    pub title: String,
    pub model: String,
    pub output: String,
    pub duration_ms: u64,
    pub usage: Usage,
}

#[derive(Debug, Clone, Serialize)]
pub struct TeamReport {
    pub goal: String,
    pub started_unix_seconds: u64,
    pub status: String,
    pub error: Option<String>,
    pub duration_ms: u64,
    pub turns: Vec<TeamTurn>,
}

impl TeamReport {
    pub fn new(goal: String) -> Self {
        Self {
            goal,
            started_unix_seconds: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            status: "running".to_owned(),
            error: None,
            duration_ms: 0,
            turns: Vec::new(),
        }
    }

    pub fn total_usage(&self) -> Usage {
        self.turns
            .iter()
            .fold(Usage::default(), |sum, turn| sum + turn.usage)
    }

    pub fn render_markdown(&self) -> String {
        let usage = self.total_usage();
        let mut content = format!(
            "# Ryoko team preview\n\n**Goal:** {}\n\n**Status:** {}\n\n**Completed roles:** {}/8\n\n**Duration:** {}ms\n\n**Reported tokens:** {} input, {} output (providers may omit usage)\n\n**Cost:** not verified; pricing is provider-dependent.\n\n> Untrusted model-generated advice, not validated or executed application code.\n\n",
            self.goal.replace('\n', " "),
            self.status,
            self.turns.len(),
            self.duration_ms,
            usage.input_tokens,
            usage.output_tokens
        );
        if let Some(error) = &self.error {
            content.push_str(&format!("**Stopped:** {}\n\n", error.replace('\n', " ")));
        }
        for turn in &self.turns {
            content.push_str(&format!(
                "## {} ({})\n\n**Model:** {}\n\n**Time:** {}ms\n\n{}\n\n---\n\n",
                turn.title, turn.role, turn.model, turn.duration_ms, turn.output
            ));
        }
        content
    }

    pub fn render_json(&self) -> io::Result<String> {
        serde_json::to_string_pretty(self).map_err(|err| io::Error::other(err.to_string()))
    }
}

/// Never auto-export proprietary model output; refuse to overwrite even symlinks.
pub fn create_private_file(path: &Path, content: &str) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    if let Err(error) = file
        .write_all(content.as_bytes())
        .and_then(|_| file.sync_all())
    {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ryoko_models::ProviderProfile;

    fn catalog() -> ProviderCatalog {
        ProviderCatalog {
            default_provider: "local".to_owned(),
            providers: vec![
                ProviderProfile {
                    id: "local".to_owned(),
                    base_url: "http://127.0.0.1:11434/v1".to_owned(),
                    api_key_env: None,
                    chat_path: None,
                    allow_insecure_http: false,
                },
                ProviderProfile {
                    id: "cloud".to_owned(),
                    base_url: "https://example.com/v1".to_owned(),
                    api_key_env: Some("RYOKO_KEY".to_owned()),
                    chat_path: None,
                    allow_insecure_http: false,
                },
            ],
        }
    }

    #[test]
    fn per_role_override_and_default_work() {
        let settings = TeamSettings::from_json(
            r#"{"default_model":"local/primary","role_models":{"ui-ux-designer":"cloud/designer"}}"#,
        ).unwrap();
        let choices = settings.check_catalog(&catalog(), None).unwrap();
        assert_eq!(choices.len(), 8);
        assert_eq!(
            choices
                .iter()
                .find(|(role, _)| role == "ui-ux-designer")
                .unwrap()
                .1,
            "cloud/designer"
        );
        assert_eq!(
            settings.model_for("qa-engineer", None).unwrap(),
            "local/primary"
        );
        assert_eq!(
            settings
                .model_for("qa-engineer", Some("local/override"))
                .unwrap(),
            "local/override"
        );
    }

    #[test]
    fn rejects_unknown_role_model_and_provider_before_calls() {
        assert!(
            TeamSettings::from_json(
                r#"{"default_model":"local/a","role_models":{"unknown-role":"local/b"}}"#
            )
            .is_err()
        );
        assert!(TeamSettings::from_json(r#"{"default_model":"bad/"}"#).is_err());
        assert!(TeamSettings::from_json(r#"{"default_model":"local/a","secret":"oops"}"#).is_err());
        let settings = TeamSettings::from_json(r#"{"default_model":"missing/model"}"#).unwrap();
        assert!(settings.check_catalog(&catalog(), None).is_err());
        assert!(
            TeamSettings::default()
                .check_catalog(&catalog(), None)
                .is_err()
        );
    }

    #[test]
    fn creates_private_report_without_overwriting() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("review.md");
        create_private_file(&path, "first").unwrap();
        assert!(create_private_file(&path, "second").is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "first");
    }

    #[test]
    fn report_renders_partial_results_and_usage() {
        let mut report = TeamReport::new("Build a todo app".to_owned());
        report.status = "failed".to_owned();
        report.error = Some("provider unreachable".to_owned());
        report.turns.push(TeamTurn {
            role: "project-manager".into(),
            title: "Project manager".into(),
            model: "local/test".into(),
            output: "Define requirements".into(),
            duration_ms: 100,
            usage: Usage::new(22, 13),
        });
        assert_eq!(report.total_usage(), Usage::new(22, 13));
        assert!(report.render_markdown().contains("Completed roles:** 1/8"));
        assert!(
            report
                .render_json()
                .unwrap()
                .contains("provider unreachable")
        );
    }
}
