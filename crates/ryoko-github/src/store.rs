//! Where the long-lived GitHub credential lives.
//!
//! Ryoko does not own a secret file. Persistence goes through **Git's own
//! credential-helper chain** (`git credential approve|fill|reject`), which is
//! the same OS-backed store — macOS Keychain, Windows Credential Manager,
//! libsecret on Linux — that Git and the GitHub CLI already use. This also
//! means an account the user has already signed in to with Git is picked up
//! automatically instead of asking for a second sign-in.
//!
//! Consequences, documented rather than hidden:
//!
//! - With no credential helper configured, `approve` stores nothing and the
//!   session stays in memory for as long as the app runs.
//! - If the configured helper is Git's plaintext `store` helper, the token is
//!   kept where that helper keeps it. That is the user's existing Git choice.
//! - Every call pins `GIT_TERMINAL_PROMPT=0` and `GCM_INTERACTIVE=never`, so a
//!   lookup can never open a prompt or block a window.
//!
//! The token is written to the helper on stdin and never appears in an
//! argument, a URL or a file this crate creates.

use std::process::Stdio;
use std::sync::Mutex;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

use crate::{GitHubError, Secret};

/// Preferred by headless and CLI use; wins over the stored credential.
pub const TOKEN_ENV: &str = "RYOKO_GITHUB_TOKEN";
/// Optional login that goes with [`TOKEN_ENV`].
pub const LOGIN_ENV: &str = "RYOKO_GITHUB_LOGIN";
pub const DEFAULT_HOST: &str = "github.com";

const MAX_CREDENTIAL_OUTPUT: usize = 16 * 1024;
const GIT_TIMEOUT_SECS: u64 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialSource {
    /// `RYOKO_GITHUB_TOKEN` in the process environment.
    Environment,
    /// Found in, or saved to, Git's credential helper.
    CredentialStore,
    /// Only in this process for as long as the app runs.
    Session,
}

#[derive(Clone, PartialEq, Eq)]
pub struct StoredCredential {
    pub token: Secret,
    pub login: String,
    pub scopes: Vec<String>,
    pub source: CredentialSource,
}

impl std::fmt::Debug for StoredCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredCredential")
            .field("login", &self.login)
            .field("scopes", &self.scopes)
            .field("source", &self.source)
            .field("token", &"<redacted>")
            .finish()
    }
}

#[async_trait]
pub trait CredentialStore: Send + Sync {
    async fn load(&self) -> Result<Option<StoredCredential>, GitHubError>;
    async fn save(&self, credential: &StoredCredential) -> Result<(), GitHubError>;
    async fn clear(&self) -> Result<(), GitHubError>;
}

fn parse_filled(output: &str) -> Option<StoredCredential> {
    let mut username = String::new();
    let mut password = String::new();
    for line in output.lines() {
        if let Some(value) = line.strip_prefix("username=") {
            username = value.trim().to_owned();
        } else if let Some(value) = line.strip_prefix("password=") {
            password = value.trim().to_owned();
        }
    }
    if password.is_empty() || password.len() > 512 {
        return None;
    }
    if username.len() > 200 || username.contains(['\0', '\n']) {
        username.clear();
    }
    Some(StoredCredential {
        token: Secret::new(password),
        login: username,
        scopes: Vec::new(),
        source: CredentialSource::CredentialStore,
    })
}

/// The host's Git credential helper, used as the OS credential store.
#[derive(Debug)]
pub struct GitCredentialStore {
    host: String,
    /// Extra `-c key=value` settings. Production leaves this empty so the
    /// user's own helper configuration applies; tests inject a scripted helper.
    config: Vec<(String, String)>,
}

impl Default for GitCredentialStore {
    fn default() -> Self {
        Self::new()
    }
}

impl GitCredentialStore {
    pub fn new() -> Self {
        Self {
            host: DEFAULT_HOST.to_owned(),
            config: Vec::new(),
        }
    }

    pub fn with_config(mut self, key: &str, value: &str) -> Self {
        self.config.push((key.to_owned(), value.to_owned()));
        self
    }

    fn invocation(&self) -> Command {
        let mut command = Command::new("git");
        for (key, value) in &self.config {
            command.arg("-c").arg(format!("{key}={value}"));
        }
        command
            .arg("credential")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_ASKPASS", "")
            .env("GCM_INTERACTIVE", "never")
            .env("LC_ALL", "C");
        command
    }

    /// Runs `git credential <action>` with the given key/value request on
    /// stdin. Returns `Ok(None)` when Git could not supply a credential.
    async fn exchange(
        &self,
        action: &'static str,
        fields: &[(&str, &str)],
    ) -> Result<Option<String>, GitHubError> {
        let mut child = self
            .invocation()
            .arg(action)
            .spawn()
            .map_err(|_| GitHubError::Git("Git is not installed or not on PATH".to_owned()))?;
        let mut request = format!("protocol=https\nhost={}\n", self.host);
        for (key, value) in fields {
            request.push_str(&format!("{key}={value}\n"));
        }
        request.push('\n');
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(request.as_bytes())
                .await
                .map_err(|_| GitHubError::Transport("Git credential helper rejected its input"))?;
        }
        let mut stdout = Vec::new();
        if let Some(stream) = child.stdout.take() {
            stream
                .take(MAX_CREDENTIAL_OUTPUT as u64)
                .read_to_end(&mut stdout)
                .await
                .map_err(|_| {
                    GitHubError::Transport("Git credential helper output was unreadable")
                })?;
        }
        let status = tokio::time::timeout(
            std::time::Duration::from_secs(GIT_TIMEOUT_SECS),
            child.wait(),
        )
        .await
        .map_err(|_| GitHubError::Git("the credential helper did not answer in time".to_owned()))?
        .map_err(|_| {
            GitHubError::Git("the credential helper did not report a status".to_owned())
        })?;
        if !status.success() {
            return Ok(None);
        }
        Ok(Some(String::from_utf8_lossy(&stdout).into_owned()))
    }
}

#[async_trait]
impl CredentialStore for GitCredentialStore {
    async fn load(&self) -> Result<Option<StoredCredential>, GitHubError> {
        let Some(output) = self.exchange("fill", &[]).await? else {
            return Ok(None);
        };
        Ok(parse_filled(&output))
    }

    async fn save(&self, credential: &StoredCredential) -> Result<(), GitHubError> {
        if credential.token.is_empty() {
            return Err(GitHubError::Unsafe("refusing to store an empty token"));
        }
        let login = if credential.login.is_empty() {
            "x-access-token"
        } else {
            credential.login.as_str()
        };
        // `approve` hands the secret to the configured helper on stdin. A
        // helper that stores nothing still exits successfully.
        self.exchange(
            "approve",
            &[("username", login), ("password", credential.token.expose())],
        )
        .await?;
        Ok(())
    }

    async fn clear(&self) -> Result<(), GitHubError> {
        // Erase the entry that is actually stored, so the helper matches it.
        let login = self
            .exchange("fill", &[])
            .await?
            .and_then(|output| parse_filled(&output))
            .filter(|credential| !credential.login.is_empty())
            .map(|credential| credential.login);
        let fields = login
            .as_deref()
            .map(|login| vec![("username", login)])
            .unwrap_or_default();
        self.exchange("reject", &fields).await?;
        Ok(())
    }
}

/// In-memory store for tests and for a session whose helper stores nothing.
#[derive(Default)]
pub struct MemoryStore {
    credential: Mutex<Option<StoredCredential>>,
}

#[async_trait]
impl CredentialStore for MemoryStore {
    async fn load(&self) -> Result<Option<StoredCredential>, GitHubError> {
        Ok(self
            .credential
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone())
    }

    async fn save(&self, credential: &StoredCredential) -> Result<(), GitHubError> {
        *self
            .credential
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(credential.clone());
        Ok(())
    }

    async fn clear(&self) -> Result<(), GitHubError> {
        *self
            .credential
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = None;
        Ok(())
    }
}

/// A token supplied by the environment, if any. Environment credentials are
/// never written to a helper; whoever set the variable owns it.
pub fn environment_credential() -> Option<StoredCredential> {
    let token = std::env::var(TOKEN_ENV)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())?;
    let login = std::env::var(LOGIN_ENV)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_default();
    Some(StoredCredential {
        token: Secret::new(token),
        login,
        scopes: Vec::new(),
        source: CredentialSource::Environment,
    })
}

/// Environment first, then the helper. Returns the credential and its origin.
pub async fn load_preferred(
    store: &dyn CredentialStore,
) -> Result<Option<StoredCredential>, GitHubError> {
    if let Some(credential) = environment_credential() {
        return Ok(Some(credential));
    }
    store.load().await
}

#[cfg(test)]
mod tests {
    use super::*;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn credential() -> StoredCredential {
        StoredCredential {
            token: Secret::new("gho_stored_secret"),
            login: "octocat".to_owned(),
            scopes: vec!["repo".to_owned()],
            source: CredentialSource::CredentialStore,
        }
    }

    #[tokio::test]
    async fn memory_store_round_trips_without_exposing_the_token() {
        let store = MemoryStore::default();
        assert!(store.load().await.unwrap().is_none());
        store.save(&credential()).await.unwrap();
        let loaded = store.load().await.unwrap().expect("saved");
        assert_eq!(loaded.login, "octocat");
        assert_eq!(loaded.token.expose(), "gho_stored_secret");
        assert!(!format!("{loaded:?}").contains("gho_stored_secret"));
        store.clear().await.unwrap();
        assert!(store.load().await.unwrap().is_none());
    }

    #[test]
    fn filled_output_is_parsed_and_bounded() {
        let parsed = parse_filled("username=octocat\npassword=gho_x\n\n").expect("credential");
        assert_eq!(parsed.login, "octocat");
        assert_eq!(parsed.token.expose(), "gho_x");
        assert!(parse_filled("username=octocat\n\n").is_none());
        assert!(parse_filled("username=a\npassword=gho_x").is_some());
    }

    /// A scripted helper that stores the request in a temp file. It runs with
    /// `sh`, exactly like the `!`-prefixed helpers Git supports.
    fn scripted_store() -> (tempfile::TempDir, GitCredentialStore, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("credential");
        let script = dir.path().join("helper.sh");
        // Git appends the operation (`store`, `erase` or `get`) as the last
        // argument, after any arguments the helper was configured with.
        std::fs::write(
            &script,
            "#!/bin/sh\nfile=\"$1\"\nfor last; do :; done\ncase \"$last\" in\n  store) cat > \"$file\" ;;\n  erase) rm -f \"$file\" ;;\n  get) [ -f \"$file\" ] && cat \"$file\" || exit 1 ;;\nesac\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let helper = format!("!sh {} {}", script.display(), file.display());
        let store = GitCredentialStore::new()
            .with_config("credential.helper", "")
            .with_config("credential.helper", &helper);
        (dir, store, file)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_git_helper_round_trips_a_credential_and_erases_it() {
        let (_dir, store, file) = scripted_store();
        assert!(store.load().await.unwrap().is_none());
        store.save(&credential()).await.unwrap();
        let stored = std::fs::read_to_string(&file).unwrap();
        assert!(stored.contains("github.com"));
        let loaded = store.load().await.unwrap().expect("stored credential");
        assert_eq!(loaded.login, "octocat");
        assert_eq!(loaded.token.expose(), "gho_stored_secret");
        assert_eq!(loaded.source, CredentialSource::CredentialStore);
        store.clear().await.unwrap();
        assert!(store.load().await.unwrap().is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_token_never_appears_in_the_helper_command_line() {
        let (_dir, store, file) = scripted_store();
        store.save(&credential()).await.unwrap();
        let stored = std::fs::read_to_string(&file).unwrap();
        // It reaches the helper on stdin, which is the only place it exists.
        assert!(stored.contains("password=gho_stored_secret"));
        assert!(!format!("{store:?}").contains("gho_stored_secret"));
    }

    #[tokio::test]
    async fn the_environment_credential_is_trimmed_and_optional() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        unsafe {
            std::env::remove_var(TOKEN_ENV);
            std::env::remove_var(LOGIN_ENV);
        }
        assert!(environment_credential().is_none());
        unsafe {
            std::env::set_var(TOKEN_ENV, "  gho_env  ");
            std::env::set_var(LOGIN_ENV, "env-user");
        }
        let found = environment_credential().expect("environment token");
        assert_eq!(found.token.expose(), "gho_env");
        assert_eq!(found.login, "env-user");
        assert_eq!(found.source, CredentialSource::Environment);
        unsafe {
            std::env::remove_var(TOKEN_ENV);
            std::env::remove_var(LOGIN_ENV);
        }
    }
}
