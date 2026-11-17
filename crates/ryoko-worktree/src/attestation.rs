//! Signed merge provenance, stored as Git notes.
//!
//! Every human-approved merge can carry an in-toto style statement that binds
//! the commit to the models that generated it, the sandbox image that validated
//! it and the humans who approved it. The statement is wrapped in a DSSE
//! envelope and signed with an SSH key (`ssh-keygen -Y sign`), so verification
//! needs only the signer's public key and no running agent.
//!
//! The signature alone is not trusted: [`verify`] also re-reads the commit from
//! the repository and requires the signed subject digest to match it, so a valid
//! envelope cannot be replayed onto a different commit.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// Git ref holding one note per attested commit.
pub const NOTES_REF: &str = "refs/notes/ryoko-attestations";
/// Namespace passed to `ssh-keygen -Y sign` and `-Y verify`.
pub const SIGN_NAMESPACE: &str = "ryoko-attestation";
pub const STATEMENT_TYPE: &str = "https://in-toto.io/Statement/v0.1";
pub const PREDICATE_TYPE: &str = "https://ryoko.dev/attestation/merge/v1";
pub const PAYLOAD_TYPE: &str = "application/vnd.in-toto+json";

const MAX_MODELS: usize = 32;
const MAX_APPROVERS: usize = 32;
const MAX_FILES: usize = 512;
const MAX_TEXT: usize = 512;

#[derive(Debug, Error)]
pub enum AttestError {
    #[error("git failed: {0}")]
    Git(String),
    #[error("signing tool failed: {0}")]
    Tool(String),
    #[error("no attestation is stored for commit {0}")]
    Missing(String),
    #[error("commit {0} does not exist")]
    UnknownCommit(String),
    #[error("attestation is malformed: {0}")]
    Malformed(String),
    #[error("attestation signature does not verify: {0}")]
    BadSignature(String),
    #[error("attestation covers {signed}, not {asked}")]
    SubjectMismatch { signed: String, asked: String },
    #[error("attestation is invalid: {0}")]
    Invalid(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// One artefact digest in the statement subject list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvenanceSubject {
    pub name: String,
    pub digest: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelProvenance {
    /// `provider/model` pair the agent ran on.
    pub model: String,
    /// Role the model played, for example `senior-developer`.
    pub role: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxProvenance {
    /// Isolation backend, for example `oci`, `microvm` or `host`.
    pub backend: String,
    /// Image reference or digest that ran the validation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationProvenance {
    pub passed: bool,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approver {
    pub name: String,
    pub email: String,
    /// RFC 3339 timestamp of the approval.
    pub approved_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Predicate {
    pub commit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub models: Vec<ModelProvenance>,
    pub sandbox: SandboxProvenance,
    pub validation: ValidationProvenance,
    pub approvers: Vec<Approver>,
    /// Changed files with their content digests.
    pub files: Vec<ProvenanceSubject>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Statement {
    #[serde(rename = "_type")]
    pub statement_type: String,
    pub subject: Vec<ProvenanceSubject>,
    #[serde(rename = "predicateType")]
    pub predicate_type: String,
    pub predicate: Predicate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signature {
    pub keyid: String,
    /// Base64 of the armored SSHSIG document.
    pub sig: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    #[serde(rename = "payloadType")]
    pub payload_type: String,
    /// Base64 of the statement JSON.
    pub payload: String,
    pub signatures: Vec<Signature>,
}

impl Envelope {
    /// Decodes and validates the statement. Nothing here executes: the payload
    /// is parsed as data and every field is bounded.
    pub fn statement(&self) -> Result<Statement, AttestError> {
        if self.payload_type != PAYLOAD_TYPE {
            return Err(AttestError::Malformed(format!(
                "unexpected payload type {}",
                self.payload_type
            )));
        }
        let bytes = base64_decode(&self.payload)
            .ok_or_else(|| AttestError::Malformed("payload is not base64".to_owned()))?;
        let statement: Statement = serde_json::from_slice(&bytes)
            .map_err(|error| AttestError::Malformed(error.to_string()))?;
        statement.validate()?;
        Ok(statement)
    }
}

impl Statement {
    pub fn validate(&self) -> Result<(), AttestError> {
        if self.statement_type != STATEMENT_TYPE {
            return Err(AttestError::Invalid(format!(
                "unexpected statement type {}",
                self.statement_type
            )));
        }
        if self.predicate_type != PREDICATE_TYPE {
            return Err(AttestError::Invalid(format!(
                "unexpected predicate type {}",
                self.predicate_type
            )));
        }
        check_sha(&self.predicate.commit)?;
        if self.subject.is_empty() {
            return Err(AttestError::Invalid("statement has no subject".to_owned()));
        }
        if self.predicate.models.len() > MAX_MODELS {
            return Err(AttestError::Invalid("too many models".to_owned()));
        }
        if self.predicate.approvers.len() > MAX_APPROVERS {
            return Err(AttestError::Invalid("too many approvers".to_owned()));
        }
        if self.predicate.files.len() > MAX_FILES {
            return Err(AttestError::Invalid("too many files".to_owned()));
        }
        if self.predicate.approvers.is_empty() {
            return Err(AttestError::Invalid(
                "a merge attestation needs at least one approver".to_owned(),
            ));
        }
        for text in [
            &self.predicate.sandbox.backend,
            &self.predicate.validation.summary,
        ] {
            check_text(text)?;
        }
        for model in &self.predicate.models {
            check_text(&model.model)?;
            check_text(&model.role)?;
        }
        for approver in &self.predicate.approvers {
            check_text(&approver.name)?;
            check_text(&approver.email)?;
            check_text(&approver.approved_at)?;
        }
        Ok(())
    }
}

fn check_text(text: &str) -> Result<(), AttestError> {
    if text.trim().is_empty() || text.len() > MAX_TEXT || text.chars().any(char::is_control) {
        return Err(AttestError::Invalid(
            "attestation text must be non-empty, printable and bounded".to_owned(),
        ));
    }
    Ok(())
}

fn check_sha(sha: &str) -> Result<(), AttestError> {
    let hex_only = |value: &str| value.bytes().all(|b| b.is_ascii_hexdigit());
    match sha.len() {
        40 if hex_only(sha) => Ok(()),
        64 if hex_only(sha) => Ok(()),
        _ => Err(AttestError::Invalid(
            "commit must be a full 40 or 64 character hexadecimal id".to_owned(),
        )),
    }
}

/// Operator input for one attestation. Everything is explicit; no field is
/// inferred from model output.
#[derive(Debug, Clone)]
pub struct AttestOptions {
    pub commit: String,
    pub branch: Option<String>,
    pub models: Vec<ModelProvenance>,
    pub sandbox: SandboxProvenance,
    pub validation: ValidationProvenance,
    pub approvers: Vec<Approver>,
    /// SSH private key used for signing. Never stored in the note.
    pub signing_key: PathBuf,
}

/// How a verifier decides which signers to trust.
#[derive(Debug, Clone)]
pub enum Trust {
    /// An OpenSSH `allowed_signers` file, plus the principal the signature must
    /// claim. The principal is always explicit, never guessed.
    AllowedSigners { path: PathBuf, identity: String },
    /// One public key plus the principal it is trusted as.
    PublicKey { path: PathBuf, identity: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified {
    pub commit: String,
    pub keyid: String,
    pub models: Vec<ModelProvenance>,
    pub sandbox: SandboxProvenance,
    pub approvers: Vec<Approver>,
    pub files: Vec<String>,
}

async fn run(mut command: Command, stdin: Option<Vec<u8>>) -> Result<Output, AttestError> {
    command
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn()?;
    if let Some(bytes) = stdin {
        let mut handle = child
            .stdin
            .take()
            .ok_or_else(|| AttestError::Tool("child has no stdin".to_owned()))?;
        handle.write_all(&bytes).await?;
        drop(handle);
    }
    Ok(child.wait_with_output().await?)
}

fn describe(program: &str, output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let text = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    let text: String = text.chars().take(400).collect();
    format!("{program} exited with {}: {text}", output.status)
}

async fn git(root: &Path, args: &[&str], stdin: Option<Vec<u8>>) -> Result<Output, AttestError> {
    let mut command = Command::new("git");
    command
        .current_dir(root)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C");
    let output = run(command, stdin).await?;
    Ok(output)
}

async fn git_ok(
    root: &Path,
    args: &[&str],
    stdin: Option<Vec<u8>>,
) -> Result<Vec<u8>, AttestError> {
    let output = git(root, args, stdin).await?;
    if !output.status.success() {
        return Err(AttestError::Git(describe("git", &output)));
    }
    Ok(output.stdout)
}

/// Resolves the full commit id, so a note can never be attached to a branch
/// name that moves later.
pub async fn resolve_commit(root: &Path, commit: &str) -> Result<String, AttestError> {
    let output = git(
        root,
        &["rev-parse", "--verify", &format!("{commit}^{{commit}}")],
        None,
    )
    .await?;
    if !output.status.success() {
        return Err(AttestError::UnknownCommit(commit.to_owned()));
    }
    let resolved = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    check_sha(&resolved)?;
    Ok(resolved)
}

async fn changed_files(root: &Path, commit: &str) -> Result<Vec<String>, AttestError> {
    // A merge commit has no single-parent diff, so compare it with its first
    // parent (what the merge brought into the branch). A root commit is compared
    // against the empty tree instead.
    let parent = git(root, &["rev-parse", &format!("{commit}^1")], None).await?;
    let raw = if parent.status.success() {
        let parent = String::from_utf8_lossy(&parent.stdout).trim().to_owned();
        git_ok(root, &["diff", "--name-only", "-z", &parent, commit], None).await?
    } else {
        git_ok(
            root,
            &[
                "diff-tree",
                "--root",
                "--no-commit-id",
                "--name-only",
                "-r",
                "-z",
                commit,
            ],
            None,
        )
        .await?
    };
    let mut files = Vec::new();
    for line in raw.split(|byte| *byte == 0) {
        let line = String::from_utf8_lossy(line);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if files.len() >= MAX_FILES {
            return Err(AttestError::Invalid(format!(
                "commit touches more than {MAX_FILES} files; attest by hand"
            )));
        }
        files.push(line.to_owned());
    }
    files.sort();
    files.dedup();
    Ok(files)
}

async fn file_digest(root: &Path, commit: &str, path: &str) -> Result<String, AttestError> {
    // `cat-file` with a tree-ish reads the blob as committed, not the worktree.
    let spec = format!("{commit}:{path}");
    let output = git(root, &["cat-file", "-p", &spec], None).await?;
    if !output.status.success() {
        // A deletion has no blob to hash; the path is still recorded.
        return Ok(sha256_hex(b""));
    }
    Ok(sha256_hex(&output.stdout))
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = chunk.get(1).map(|b| u32::from(*b)).unwrap_or(0);
        let b2 = chunk.get(2).map(|b| u32::from(*b)).unwrap_or(0);
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[(triple >> 18) as usize & 0x3f] as char);
        out.push(TABLE[(triple >> 12) as usize & 0x3f] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(triple >> 6) as usize & 0x3f] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[triple as usize & 0x3f] as char
        } else {
            '='
        });
    }
    out
}

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let text = text.trim();
    if !text.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let value = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => u32::from(c - b'A'),
            b'a'..=b'z' => u32::from(c - b'a') + 26,
            b'0'..=b'9' => u32::from(c - b'0') + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    };
    let bytes = text.as_bytes();
    for (index, chunk) in bytes.chunks(4).enumerate() {
        let pad = chunk.iter().filter(|c| **c == b'=').count();
        if pad > 2 || (pad > 0 && index != bytes.len() / 4 - 1) {
            return None;
        }
        let mut triple = 0u32;
        for (offset, byte) in chunk.iter().enumerate() {
            let digit = if *byte == b'=' { 0 } else { value(*byte)? };
            triple |= digit << (18 - 6 * offset);
        }
        // Canonical encodings zero the unused low bits; reject anything else so
        // two different strings cannot decode to the same bytes.
        if (pad == 2 && triple & 0x0000_ffff != 0) || (pad == 1 && triple & 0x0000_00ff != 0) {
            return None;
        }
        out.push((triple >> 16) as u8);
        if pad < 2 {
            out.push((triple >> 8) as u8);
        }
        if pad < 1 {
            out.push(triple as u8);
        }
    }
    Some(out)
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

/// Derives the signer's key id from the public half of a private key, so the
/// note names the key without ever touching the secret again.
async fn key_id(private_key: &Path) -> Result<String, AttestError> {
    let output = run(
        {
            let mut command = Command::new("ssh-keygen");
            command.args(["-lf"]).arg(private_key);
            command
        },
        None,
    )
    .await?;
    if !output.status.success() {
        return Err(AttestError::Tool(describe("ssh-keygen -lf", &output)));
    }
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let id = stdout
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| AttestError::Tool("ssh-keygen printed no fingerprint".to_owned()))?;
    Ok(id.to_owned())
}

/// Exports the public half of a signing key, so an operator can publish what a
/// verifier needs. The private key is never written anywhere by this module.
pub async fn public_key(private_key: &Path) -> Result<String, AttestError> {
    let output = run(
        {
            let mut command = Command::new("ssh-keygen");
            command.args(["-y", "-f"]).arg(private_key);
            command
        },
        None,
    )
    .await?;
    if !output.status.success() {
        return Err(AttestError::Tool(describe("ssh-keygen -y", &output)));
    }
    let key = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if key.is_empty() {
        return Err(AttestError::Tool("ssh-keygen printed no key".to_owned()));
    }
    Ok(key)
}

/// Signs `payload` in `namespace` and returns the armored signature text.
async fn sign(payload: &[u8], key: &Path) -> Result<String, AttestError> {
    let dir = std::env::temp_dir().join(format!("ryoko-sign-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir)?;
    let payload_path = dir.join("payload.json");
    std::fs::write(&payload_path, payload)?;
    let result = async {
        let output = run(
            {
                let mut command = Command::new("ssh-keygen");
                command
                    .args(["-Y", "sign", "-f"])
                    .arg(key)
                    .args(["-n", SIGN_NAMESPACE])
                    .arg(&payload_path);
                command
            },
            None,
        )
        .await?;
        if !output.status.success() {
            return Err(AttestError::Tool(describe("ssh-keygen -Y sign", &output)));
        }
        let signature_path = payload_path.with_extension("json.sig");
        std::fs::read_to_string(&signature_path).map_err(AttestError::Io)
    }
    .await;
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// Verifies an armored SSHSIG over `payload` with the given trust anchor.
async fn verify_signature(
    payload: &[u8],
    signature: &[u8],
    trust: &Trust,
) -> Result<(), AttestError> {
    let dir = std::env::temp_dir().join(format!("ryoko-verify-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir)?;
    let payload_path = dir.join("payload.json");
    let signature_path = dir.join("payload.sig");
    std::fs::write(&payload_path, payload)?;
    std::fs::write(&signature_path, signature)?;

    let (signers_path, identity) = match trust {
        Trust::AllowedSigners { path, identity } => {
            let text = std::fs::read_to_string(path)?;
            if text.trim().is_empty() {
                let _ = std::fs::remove_dir_all(&dir);
                return Err(AttestError::BadSignature(
                    "allowed signers file is empty".to_owned(),
                ));
            }
            (path.clone(), identity.clone())
        }
        Trust::PublicKey { path, identity } => {
            let key = std::fs::read_to_string(path)?;
            if key.trim().is_empty() {
                let _ = std::fs::remove_dir_all(&dir);
                return Err(AttestError::BadSignature("public key is empty".to_owned()));
            }
            let allowed = dir.join("allowed_signers");
            let line = format!("{identity} {}\n", key.trim());
            std::fs::write(&allowed, line)?;
            (allowed, identity.clone())
        }
    };

    let output = run(
        {
            let mut command = Command::new("ssh-keygen");
            command
                .args(["-Y", "verify", "-f"])
                .arg(&signers_path)
                .args(["-n", SIGN_NAMESPACE, "-s"])
                .arg(&signature_path);
            command.args(["-I", &identity]);
            command.arg("-");
            command
        },
        Some(payload.to_vec()),
    )
    .await;
    let _ = std::fs::remove_dir_all(&dir);
    let output = output?;
    if !output.status.success() {
        return Err(AttestError::BadSignature(describe(
            "ssh-keygen -Y verify",
            &output,
        )));
    }
    Ok(())
}

async fn store_note(root: &Path, commit: &str, note: &str) -> Result<(), AttestError> {
    git_ok(
        root,
        &[
            "notes",
            &format!("--ref={NOTES_REF}"),
            "add",
            "-f",
            "-F",
            "-",
            commit,
        ],
        Some(note.as_bytes().to_vec()),
    )
    .await
    .map(|_| ())
}

/// Builds, signs and stores an attestation for one commit.
pub async fn create(root: &Path, options: &AttestOptions) -> Result<Envelope, AttestError> {
    let commit = resolve_commit(root, &options.commit).await?;
    if options.approvers.is_empty() {
        return Err(AttestError::Invalid(
            "a merge attestation needs at least one approver".to_owned(),
        ));
    }
    let files = changed_files(root, &commit).await?;
    let mut subjects = Vec::with_capacity(files.len());
    for path in &files {
        let mut digest = BTreeMap::new();
        digest.insert("sha256".to_owned(), file_digest(root, &commit, path).await?);
        subjects.push(ProvenanceSubject {
            name: path.clone(),
            digest,
        });
    }
    let mut commit_digest = BTreeMap::new();
    commit_digest.insert("gitCommit".to_owned(), commit.clone());
    let statement = Statement {
        statement_type: STATEMENT_TYPE.to_owned(),
        subject: vec![ProvenanceSubject {
            name: format!("git+commit:{commit}"),
            digest: commit_digest,
        }],
        predicate_type: PREDICATE_TYPE.to_owned(),
        predicate: Predicate {
            commit: commit.clone(),
            branch: options.branch.clone(),
            models: options.models.clone(),
            sandbox: options.sandbox.clone(),
            validation: options.validation.clone(),
            approvers: options.approvers.clone(),
            files: subjects,
        },
    };
    statement.validate()?;
    let payload = serde_json::to_vec(&statement)
        .map_err(|error| AttestError::Malformed(error.to_string()))?;
    let signature = sign(&payload, &options.signing_key).await?;
    let envelope = Envelope {
        payload_type: PAYLOAD_TYPE.to_owned(),
        payload: base64_encode(&payload),
        signatures: vec![Signature {
            keyid: key_id(&options.signing_key).await?,
            sig: base64_encode(signature.as_bytes()),
        }],
    };
    let note = serde_json::to_string_pretty(&envelope)
        .map_err(|error| AttestError::Malformed(error.to_string()))?;
    store_note(root, &commit, &note).await?;
    Ok(envelope)
}

/// Reads the stored envelope without trusting it.
pub async fn read(root: &Path, commit: &str) -> Result<Envelope, AttestError> {
    let commit = resolve_commit(root, commit).await?;
    let output = git(
        root,
        &["notes", &format!("--ref={NOTES_REF}"), "show", &commit],
        None,
    )
    .await?;
    if !output.status.success() {
        return Err(AttestError::Missing(commit));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    if text.trim().is_empty() {
        return Err(AttestError::Malformed("stored note is empty".to_owned()));
    }
    serde_json::from_str(&text).map_err(|error| AttestError::Malformed(error.to_string()))
}

/// Verifies the stored attestation for `commit`: the signature must verify
/// against `trust`, and the signed subject must name that exact commit.
pub async fn verify(root: &Path, commit: &str, trust: &Trust) -> Result<Verified, AttestError> {
    let commit = resolve_commit(root, commit).await?;
    let envelope = read(root, &commit).await?;
    if envelope.signatures.len() != 1 {
        return Err(AttestError::Malformed(format!(
            "expected exactly one signature, found {}",
            envelope.signatures.len()
        )));
    }
    let signature = &envelope.signatures[0];
    let payload = base64_decode(&envelope.payload)
        .ok_or_else(|| AttestError::Malformed("payload is not base64".to_owned()))?;
    let raw_signature = base64_decode(&signature.sig)
        .ok_or_else(|| AttestError::Malformed("signature is not base64".to_owned()))?;
    verify_signature(&payload, &raw_signature, trust).await?;

    let statement: Statement = serde_json::from_slice(&payload)
        .map_err(|error| AttestError::Malformed(error.to_string()))?;
    statement.validate()?;
    let signed = &statement.predicate.commit;
    if signed != &commit {
        return Err(AttestError::SubjectMismatch {
            signed: signed.clone(),
            asked: commit,
        });
    }
    let binds_commit = statement.subject.iter().any(|subject| {
        subject
            .digest
            .get("gitCommit")
            .map(|value| value == &commit)
            .unwrap_or(false)
    });
    if !binds_commit {
        return Err(AttestError::SubjectMismatch {
            signed: "no matching subject digest".to_owned(),
            asked: commit,
        });
    }
    Ok(Verified {
        commit,
        keyid: signature.keyid.clone(),
        models: statement.predicate.models,
        sandbox: statement.predicate.sandbox,
        approvers: statement.predicate.approvers,
        files: statement
            .predicate
            .files
            .into_iter()
            .map(|file| file.name)
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    struct Repo {
        dir: TempDir,
        /// Keys live outside the repository: a `git add -A` inside a test must
        /// never be able to commit a private key.
        keys: TempDir,
        key: PathBuf,
        public: PathBuf,
    }

    async fn tool_ok(mut command: Command) -> String {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let output = command.output().await.expect("command runs");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    async fn repo() -> Repo {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        let mut init = Command::new("git");
        init.current_dir(root).args(["init", "-q", "-b", "main"]);
        tool_ok(init).await;
        for (key, value) in [
            ("user.name", "Test"),
            ("user.email", "test@example.com"),
            ("commit.gpgsign", "false"),
        ] {
            let mut config = Command::new("git");
            config.current_dir(root).args(["config", key, value]);
            tool_ok(config).await;
        }
        std::fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();
        let mut add = Command::new("git");
        add.current_dir(root).args(["add", "-A"]);
        tool_ok(add).await;
        let mut commit = Command::new("git");
        commit
            .current_dir(root)
            .args(["commit", "-q", "-m", "initial"]);
        tool_ok(commit).await;

        let keys = TempDir::new().unwrap();
        let key = keys.path().join("signing-key");
        let mut generate = Command::new("ssh-keygen");
        generate
            .args(["-q", "-t", "ed25519", "-N", "", "-C", "ryoko-test", "-f"])
            .arg(&key);
        tool_ok(generate).await;
        let public = keys.path().join("signing-key.pub");
        Repo {
            dir,
            keys,
            key,
            public,
        }
    }

    fn options(commit: String, key: &Path) -> AttestOptions {
        AttestOptions {
            commit,
            branch: Some("main".to_owned()),
            models: vec![ModelProvenance {
                model: "local/qwen2.5-coder".to_owned(),
                role: "senior-developer".to_owned(),
            }],
            sandbox: SandboxProvenance {
                backend: "oci".to_owned(),
                image: Some("sha256:abcd".to_owned()),
            },
            validation: ValidationProvenance {
                passed: true,
                summary: "pre-flight passed; security audit: clean".to_owned(),
            },
            approvers: vec![Approver {
                name: "Pourush Nair".to_owned(),
                email: "pourushnair6@gmail.com".to_owned(),
                approved_at: "2026-11-17T10:00:00Z".to_owned(),
            }],
            signing_key: key.to_path_buf(),
        }
    }

    async fn head(root: &Path) -> String {
        let mut command = Command::new("git");
        command.current_dir(root).args(["rev-parse", "HEAD"]);
        tool_ok(command).await
    }

    #[test]
    fn base64_round_trips_and_rejects_garbage() {
        for length in 0..40usize {
            let bytes: Vec<u8> = (0..length).map(|index| (index * 37 % 251) as u8).collect();
            let encoded = base64_encode(&bytes);
            assert_eq!(base64_decode(&encoded).unwrap(), bytes);
        }
        assert!(base64_decode("not base64!!").is_none());
        assert!(
            base64_decode("QQ=").is_none(),
            "length must be a multiple of 4"
        );
        assert!(base64_decode("QR==").is_none(), "non-zero padding bits");
        assert_eq!(base64_decode("QQ==").unwrap(), b"A".to_vec());
    }

    #[test]
    fn statements_reject_unbounded_or_anonymous_input() {
        let statement = Statement {
            statement_type: STATEMENT_TYPE.to_owned(),
            subject: vec![ProvenanceSubject {
                name: "git+commit:abc".to_owned(),
                digest: BTreeMap::new(),
            }],
            predicate_type: PREDICATE_TYPE.to_owned(),
            predicate: Predicate {
                commit: "a".repeat(40),
                branch: None,
                models: Vec::new(),
                sandbox: SandboxProvenance {
                    backend: "oci".to_owned(),
                    image: None,
                },
                validation: ValidationProvenance {
                    passed: true,
                    summary: "ok".to_owned(),
                },
                approvers: Vec::new(),
                files: Vec::new(),
            },
        };
        assert!(
            statement.validate().is_err(),
            "no approver must be rejected"
        );
        let mut signed = statement.clone();
        signed.predicate.approvers.push(Approver {
            name: "A".to_owned(),
            email: "a@example.com".to_owned(),
            approved_at: "2026-11-17T10:00:00Z".to_owned(),
        });
        assert!(signed.validate().is_ok());
        let mut short = signed.clone();
        short.predicate.commit = "abc".to_owned();
        assert!(short.validate().is_err());
        let mut control = signed;
        control.predicate.validation.summary = "bad\u{7}".to_owned();
        assert!(control.validate().is_err());
    }

    #[tokio::test]
    async fn signs_stores_and_verifies_against_the_commit() {
        let repo = repo().await;
        let root = repo.dir.path();
        let commit = head(root).await;
        let envelope = create(root, &options(commit.clone(), &repo.key))
            .await
            .unwrap();
        assert_eq!(envelope.payload_type, PAYLOAD_TYPE);

        let stored = read(root, &commit).await.unwrap();
        assert_eq!(stored, envelope);

        let verified = verify(
            root,
            &commit,
            &Trust::PublicKey {
                path: repo.public.clone(),
                identity: "ryoko-test".to_owned(),
            },
        )
        .await
        .unwrap();
        assert_eq!(verified.commit, commit);
        assert!(verified.keyid.starts_with("SHA256:"));
        assert_eq!(verified.models[0].model, "local/qwen2.5-coder");
        assert_eq!(verified.sandbox.image.as_deref(), Some("sha256:abcd"));
        assert_eq!(verified.approvers.len(), 1);
        assert_eq!(verified.files, vec!["main.rs".to_owned()]);

        // The allowed-signers form trusts the same key under a named principal.
        let allowed = repo.keys.path().join("allowed_signers");
        let key = public_key(&repo.key).await.unwrap();
        std::fs::write(&allowed, format!("pourush {key}\n")).unwrap();
        let verified = verify(
            root,
            &commit,
            &Trust::AllowedSigners {
                path: allowed.clone(),
                identity: "pourush".to_owned(),
            },
        )
        .await
        .unwrap();
        assert_eq!(verified.commit, commit);
        // A different principal in the same file is not the signer.
        let error = verify(
            root,
            &commit,
            &Trust::AllowedSigners {
                path: allowed,
                identity: "someone-else".to_owned(),
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(error, AttestError::BadSignature(_)), "{error}");
    }

    #[tokio::test]
    async fn a_second_commit_cannot_reuse_the_first_attestation() {
        let repo = repo().await;
        let root = repo.dir.path();
        let first = head(root).await;
        create(root, &options(first.clone(), &repo.key))
            .await
            .unwrap();

        std::fs::write(root.join("second.rs"), "// later\n").unwrap();
        let mut add = Command::new("git");
        add.current_dir(root).args(["add", "-A"]);
        tool_ok(add).await;
        let mut commit = Command::new("git");
        commit
            .current_dir(root)
            .args(["commit", "-q", "-m", "later"]);
        tool_ok(commit).await;
        let second = head(root).await;

        let trust = Trust::PublicKey {
            path: repo.public.clone(),
            identity: "ryoko-test".to_owned(),
        };
        assert!(matches!(
            verify(root, &second, &trust).await,
            Err(AttestError::Missing(_))
        ));
        // Copying the note onto the new commit does not help: the payload is
        // bound to the first commit.
        let note = read(root, &first).await.unwrap();
        store_note(root, &second, &serde_json::to_string(&note).unwrap())
            .await
            .unwrap();
        let error = verify(root, &second, &trust).await.unwrap_err();
        assert!(
            matches!(error, AttestError::SubjectMismatch { .. }),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_merge_attestation_records_the_files_the_merge_brought_in() {
        let repo = repo().await;
        let root = repo.dir.path();
        let mut branch = Command::new("git");
        branch
            .current_dir(root)
            .args(["checkout", "-q", "-b", "feature"]);
        tool_ok(branch).await;
        std::fs::write(root.join("feature.rs"), "// feature\n").unwrap();
        let mut add = Command::new("git");
        add.current_dir(root).args(["add", "-A"]);
        tool_ok(add).await;
        let mut commit = Command::new("git");
        commit
            .current_dir(root)
            .args(["commit", "-q", "-m", "feature work"]);
        tool_ok(commit).await;
        let mut back = Command::new("git");
        back.current_dir(root).args(["checkout", "-q", "main"]);
        tool_ok(back).await;
        let mut merge = Command::new("git");
        merge
            .current_dir(root)
            .args(["merge", "-q", "--no-ff", "feature", "-m", "merge feature"]);
        tool_ok(merge).await;
        let commit = head(root).await;

        create(root, &options(commit.clone(), &repo.key))
            .await
            .unwrap();
        let verified = verify(
            root,
            &commit,
            &Trust::PublicKey {
                path: repo.public.clone(),
                identity: "ryoko-test".to_owned(),
            },
        )
        .await
        .unwrap();
        assert_eq!(verified.files, vec!["feature.rs".to_owned()]);
    }

    #[tokio::test]
    async fn an_untrusted_key_is_rejected() {
        let repo = repo().await;
        let root = repo.dir.path();
        let commit = head(root).await;
        create(root, &options(commit.clone(), &repo.key))
            .await
            .unwrap();

        let other = repo.keys.path().join("other-key");
        let mut generate = Command::new("ssh-keygen");
        generate
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(&other);
        tool_ok(generate).await;
        let error = verify(
            root,
            &commit,
            &Trust::PublicKey {
                path: repo.keys.path().join("other-key.pub"),
                identity: "ryoko-test".to_owned(),
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(error, AttestError::BadSignature(_)), "{error}");
    }

    #[tokio::test]
    async fn tampering_with_the_payload_breaks_the_signature() {
        let repo = repo().await;
        let root = repo.dir.path();
        let commit = head(root).await;
        let mut envelope = create(root, &options(commit.clone(), &repo.key))
            .await
            .unwrap();

        let mut statement = envelope.statement().unwrap();
        statement.predicate.approvers[0].name = "Someone Else".to_owned();
        let payload = serde_json::to_vec(&statement).unwrap();
        envelope.payload = base64_encode(&payload);
        store_note(root, &commit, &serde_json::to_string(&envelope).unwrap())
            .await
            .unwrap();

        let error = verify(
            root,
            &commit,
            &Trust::PublicKey {
                path: repo.public.clone(),
                identity: "ryoko-test".to_owned(),
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(error, AttestError::BadSignature(_)), "{error}");
    }

    #[tokio::test]
    async fn unknown_commits_and_missing_notes_are_distinct_errors() {
        let repo = repo().await;
        let root = repo.dir.path();
        assert!(matches!(
            read(root, "does-not-exist").await,
            Err(AttestError::UnknownCommit(_))
        ));
        let commit = head(root).await;
        assert!(matches!(
            read(root, &commit).await,
            Err(AttestError::Missing(_))
        ));
    }
}
