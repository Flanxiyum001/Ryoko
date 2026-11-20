//! Capture of rendered routes for UI-touching changes.
//!
//! The operator names the command that serves the app; the sandbox image supplies
//! the headless browser and `verify-ui.mjs`. Captures are written into the task's
//! artifact mount, which is where the UI/UX review looks for them. Nothing here
//! gates a change: no capture simply means no visual evidence, and the validator
//! says so in its logs.

use std::path::Path;
use std::time::Duration;

use ryoko_core::model::ProposedChange;
use ryoko_sandbox::{ARTIFACT_MOUNT, BuildSandbox};

/// Harness path inside a `ryoko sandbox init` image.
pub const DEFAULT_HARNESS: &str = "/opt/ryoko/verify-ui.mjs";
/// Loopback URL the harness polls; the harness refuses anything else.
pub const DEFAULT_URL: &str = "http://127.0.0.1:4173";

const MAX_ROUTES: usize = 25;
const MAX_ROUTE_LENGTH: usize = 200;

/// Extensions whose change is worth rendering. A change that touches none of
/// them never starts a browser.
const UI_EXTENSIONS: [&str; 13] = [
    "html", "htm", "css", "scss", "sass", "less", "jsx", "tsx", "vue", "svelte", "astro", "mdx",
    "styl",
];

/// An operator-configured dev server and the routes to render.
#[derive(Debug, Clone)]
pub struct UiCapture {
    dev_command: String,
    routes: Vec<String>,
    harness: String,
    url: String,
}

impl UiCapture {
    pub fn new(dev_command: impl Into<String>) -> Self {
        Self {
            dev_command: dev_command.into(),
            routes: Vec::new(),
            harness: DEFAULT_HARNESS.to_owned(),
            url: DEFAULT_URL.to_owned(),
        }
    }

    /// Adds a route. Repeated routes are collapsed, because a duplicate capture
    /// would only double the prompt.
    pub fn with_route(mut self, route: impl Into<String>) -> Self {
        let route = route.into();
        if !self.routes.iter().any(|kept| kept == &route) {
            self.routes.push(route);
        }
        self
    }

    /// Redirects the harness. Production images use [`DEFAULT_HARNESS`]; tests
    /// point at a stand-in so they exercise the invocation, not a browser.
    #[cfg(test)]
    pub fn with_harness(mut self, harness: impl Into<String>) -> Self {
        self.harness = harness.into();
        self
    }

    pub fn routes(&self) -> &[String] {
        &self.routes
    }

    /// Rejects what the harness cannot take safely: an empty command, routes that
    /// are not absolute paths, and anything that would need shell quoting.
    pub fn validate(&self) -> Result<(), String> {
        if self.dev_command.trim().is_empty() {
            return Err("the UI dev command cannot be empty".to_owned());
        }
        if self.dev_command.chars().any(char::is_control) {
            return Err("the UI dev command must not contain control characters".to_owned());
        }
        if self.routes.len() > MAX_ROUTES {
            return Err(format!("at most {MAX_ROUTES} UI routes can be captured"));
        }
        for route in &self.routes {
            if !route.starts_with('/') {
                return Err(format!("UI route `{route}` must start with `/`"));
            }
            if route.len() > MAX_ROUTE_LENGTH {
                return Err(format!(
                    "UI route is longer than {MAX_ROUTE_LENGTH} characters"
                ));
            }
            if route
                .chars()
                .any(|ch| ch.is_whitespace() || ch.is_control() || matches!(ch, ',' | '\'' | '"'))
            {
                return Err(format!(
                    "UI route `{route}` must not contain whitespace, quotes or commas"
                ));
            }
            if route.split('/').any(|segment| segment == "..") {
                return Err(format!("UI route `{route}` must not contain `..`"));
            }
        }
        Ok(())
    }

    /// The shell command run inside the sandbox. The dev command is operator
    /// input, so it is single-quoted rather than interpolated.
    pub fn command(&self) -> Result<String, String> {
        self.validate()?;
        let mut routes = self.routes.clone();
        if routes.is_empty() {
            routes.push("/".to_owned());
        }
        Ok(format!(
            "node {} --dev-command {} --url {} --routes {} --out {} --allow-issues",
            shell_quote(&self.harness),
            shell_quote(self.dev_command.trim()),
            shell_quote(&self.url),
            shell_quote(&routes.join(",")),
            shell_quote(ARTIFACT_MOUNT)
        ))
    }

    /// Whether any staged file could change what a browser renders.
    pub fn touches_ui(&self, changes: &[ProposedChange]) -> bool {
        changes.iter().any(|change| {
            Path::new(&change.path)
                .extension()
                .and_then(|extension| extension.to_str())
                .map(str::to_ascii_lowercase)
                .is_some_and(|extension| UI_EXTENSIONS.contains(&extension.as_str()))
        })
    }

    /// Runs the harness in the task's sandbox. `sandbox` must already carry the
    /// task's artifact mount, because that is what `--out` resolves to.
    pub async fn capture(
        &self,
        sandbox: &BuildSandbox,
        root: &Path,
        timeout: Duration,
    ) -> Result<Vec<String>, String> {
        let command = self.command()?;
        let output = sandbox
            .run(&command, root, timeout)
            .await
            .map_err(|error| error.to_string())?;
        let lines = summarize(&output);
        if !output.status.success() {
            let status = output
                .status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "signal".to_owned());
            return Err(format!(
                "verify-ui exited {status} ({}): {}",
                self.routes().join(", "),
                lines.last().cloned().unwrap_or_default()
            ));
        }
        Ok(lines)
    }
}

/// Single-quote for POSIX `sh`, which is what the sandbox runs.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

fn summarize(output: &std::process::Output) -> Vec<String> {
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let mut lines = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| format!("visual capture: {line}"))
        .collect::<Vec<_>>();
    lines.truncate(20);
    lines
}

#[cfg(unix)]
#[cfg(test)]
mod tests {
    use super::*;
    use ryoko_sandbox::BuildBackend;
    use tempfile::TempDir;

    fn change(path: &str) -> ProposedChange {
        ProposedChange::new(path, "x")
    }

    #[test]
    fn only_rendered_asset_types_trigger_a_capture() {
        let capture = UiCapture::new("npm run dev");
        assert!(capture.touches_ui(&[change("app/page.tsx")]));
        assert!(capture.touches_ui(&[change("styles/main.CSS")]));
        assert!(!capture.touches_ui(&[
            change("src/lib.rs"),
            change("README.md"),
            change("Cargo.toml"),
        ]));
    }

    #[test]
    fn the_command_single_quotes_the_operator_dev_command() {
        let capture = UiCapture::new("sh -c 'npm run dev -- --port 3000'").with_route("/pricing");
        let command = capture.command().unwrap();
        assert!(
            command.starts_with("node '/opt/ryoko/verify-ui.mjs'"),
            "{command}"
        );
        assert!(
            command.contains(r"--dev-command 'sh -c '\''npm run dev -- --port 3000'\'''"),
            "{command}"
        );
        assert!(command.contains("--routes '/pricing'"), "{command}");
        assert!(command.contains("--out '/ryoko-artifacts'"), "{command}");
        assert!(command.contains("--allow-issues"), "{command}");
    }

    #[test]
    fn a_capture_without_routes_reviews_the_root() {
        let command = UiCapture::new("npm start").command().unwrap();
        assert!(command.contains("--routes '/'"), "{command}");
    }

    #[test]
    fn routes_and_commands_are_validated_before_anything_runs() {
        assert!(UiCapture::new("  ").validate().is_err());
        assert!(
            UiCapture::new("npm start")
                .with_route("app")
                .validate()
                .is_err()
        );
        assert!(
            UiCapture::new("npm start")
                .with_route("/a b")
                .validate()
                .is_err()
        );
        assert!(
            UiCapture::new("npm start")
                .with_route("/a';rm -rf /")
                .validate()
                .is_err()
        );
        assert!(
            UiCapture::new("npm start")
                .with_route("/../etc/passwd")
                .validate()
                .is_err()
        );
        assert!(
            UiCapture::new("npm start")
                .with_route("/pricing")
                .validate()
                .is_ok()
        );
    }

    #[tokio::test]
    async fn a_successful_harness_runs_in_the_worktree_and_writes_to_the_mount() {
        let dir = TempDir::new().unwrap();
        let harness = dir.path().join("verify-ui.mjs");
        // A stand-in for the image's harness: it records the arguments it was
        // given and echoes a summary, so this test covers the invocation and the
        // working directory. The `/ryoko-artifacts` mount itself is created by
        // the sandbox, not by this crate.
        std::fs::write(
            &harness,
            "import { writeFileSync } from 'node:fs';\n\
             writeFileSync('captured.txt', process.argv.slice(2).join('\\n'));\n\
             console.log('captured routes');\n",
        )
        .unwrap();
        let sandbox = BuildSandbox::new(BuildBackend::UnsafeHost, None);
        let capture = UiCapture::new("npm run dev")
            .with_route("/pricing")
            .with_harness(harness.to_str().unwrap());

        let lines = capture
            .capture(&sandbox, dir.path(), Duration::from_secs(30))
            .await
            .unwrap();

        assert!(
            lines.iter().any(|line| line.contains("captured routes")),
            "{lines:?}"
        );
        let recorded = std::fs::read_to_string(dir.path().join("captured.txt")).unwrap();
        assert!(recorded.contains("/pricing"), "{recorded}");
        assert!(recorded.contains("/ryoko-artifacts"), "{recorded}");
    }

    #[tokio::test]
    async fn a_failing_harness_is_reported_not_raised_as_a_validation_error() {
        let dir = TempDir::new().unwrap();
        let harness = dir.path().join("verify-ui.mjs");
        std::fs::write(
            &harness,
            "console.error('cannot launch chromium'); process.exit(2);",
        )
        .unwrap();
        let sandbox = BuildSandbox::new(BuildBackend::UnsafeHost, None);
        let capture = UiCapture::new("true").with_harness(harness.to_str().unwrap());

        let error = capture
            .capture(&sandbox, dir.path(), Duration::from_secs(30))
            .await
            .unwrap_err();

        assert!(error.contains("exited 2"), "{error}");
    }
}
