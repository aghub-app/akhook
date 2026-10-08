//! One-time approvals for `ask` rules on agents whose hooks cannot ask the
//! user themselves (Codex). A blocked call leaves a request; whoever asked the
//! user grants it; the same call retried within the grant's lifetime passes
//! once.

use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use directories::BaseDirs;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::model::{Candidate, RuleHit, ToolAttempt};

pub const STATE_DIR_ENV: &str = "AKHOOK_STATE_DIR";
const GRANT_LIFETIME: Duration = Duration::from_secs(600);
/// Requests nobody answered are dropped after a day.
const REQUEST_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);
const DEFAULT_INSTRUCTION: &str = "This call needs the user's approval (request {request_id}). \
    Ask the user to approve it; after they run `akhook approval grant {request_id}`, \
    retry exactly the same call.";

#[derive(Serialize, Deserialize)]
pub struct Request {
    pub id: String,
    pub cwd: PathBuf,
    pub rules: Vec<RequestRule>,
    pub candidates: Vec<Candidate>,
}

#[derive(Serialize, Deserialize)]
pub struct RequestRule {
    pub id: String,
    pub message: String,
}

#[derive(Serialize, Deserialize)]
struct Grant {
    expires_at: u64,
}

pub enum Outcome {
    /// A grant for this exact call existed and has been used up.
    Granted,
    /// No grant: a request was recorded; the text tells the agent what to do.
    Requested(String),
}

/// The same rules hitting the same call in the same directory always yield
/// the same request id, so a retry finds the grant made for it.
pub fn check(
    attempt: &ToolAttempt,
    hits: &[RuleHit],
    instruction: Option<&str>,
) -> Result<Outcome> {
    let request = Request {
        id: String::new(),
        cwd: attempt.cwd.clone(),
        rules: hits
            .iter()
            .map(|hit| RequestRule {
                id: hit.id.clone(),
                message: hit.message.clone(),
            })
            .collect(),
        candidates: attempt.candidates.clone(),
    };
    let mut ids: Vec<_> = hits.iter().map(|hit| hit.id.as_str()).collect();
    ids.sort_unstable();
    let key = serde_json::to_vec(&(&request.cwd, &ids, &request.candidates))?;
    let id = Sha256::digest(&key)
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let dir = state_dir()?;
    sweep(&dir);
    if take_grant(&dir.join("grants").join(format!("{id}.json")))? {
        return Ok(Outcome::Granted);
    }
    write_json(
        &request_path(&dir, &id),
        &Request {
            id: id.clone(),
            ..request
        },
    )?;
    Ok(Outcome::Requested(
        instruction
            .unwrap_or(DEFAULT_INSTRUCTION)
            .replace("{request_id}", &id),
    ))
}

pub fn show(id: &str) -> Result<Request> {
    let path = request_path(&state_dir()?, &valid(id)?);
    let text = fs::read_to_string(&path).with_context(|| format!("no pending request {id}"))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

pub fn grant(id: &str) -> Result<()> {
    let id = valid(id)?;
    let dir = state_dir()?;
    let request = request_path(&dir, &id);
    if !request.is_file() {
        bail!("no pending request {id}");
    }
    let expires_at = (now() + GRANT_LIFETIME).as_secs();
    write_json(
        &dir.join("grants").join(format!("{id}.json")),
        &Grant { expires_at },
    )?;
    fs::remove_file(&request).with_context(|| format!("removing {}", request.display()))
}

/// Removes expired grants and stale requests. Each hook process is short
/// lived, so this runs whenever an `ask` rule hits. Best effort: a failed
/// cleanup must not decide the tool call.
fn sweep(dir: &Path) {
    for (folder, lifetime) in [("grants", GRANT_LIFETIME), ("requests", REQUEST_LIFETIME)] {
        let Ok(entries) = fs::read_dir(dir.join(folder)) else {
            continue;
        };
        for entry in entries.flatten() {
            let stale = entry
                .metadata()
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age >= lifetime);
            if stale {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

fn take_grant(path: &Path) -> Result<bool> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    // Whoever removes the file uses the grant; a concurrent retry does not.
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).with_context(|| format!("removing {}", path.display())),
    }
    let grant: Grant =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok(now().as_secs() < grant.expires_at)
}

fn state_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os(STATE_DIR_ENV).filter(|dir| !dir.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    let dirs = BaseDirs::new().context("cannot locate user state directory")?;
    Ok(dirs
        .state_dir()
        .unwrap_or_else(|| dirs.data_local_dir())
        .join("akhook"))
}

fn request_path(dir: &Path, id: &str) -> PathBuf {
    dir.join("requests").join(format!("{id}.json"))
}

fn valid(id: &str) -> Result<String> {
    if id.len() != 16 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("invalid request id {id}");
    }
    Ok(id.to_ascii_lowercase())
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let dir = path.parent().expect("state files live in a directory");
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, serde_json::to_vec(value)?)
        .with_context(|| format!("writing {}", temporary.display()))?;
    fs::rename(&temporary, path).with_context(|| format!("writing {}", path.display()))
}

fn now() -> Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::fs::File;

    use super::*;

    #[test]
    fn sweep_removes_expired_grants_and_stale_requests_only() {
        let dir = std::env::temp_dir().join(format!("akhook-sweep-{}", std::process::id()));
        let file = |folder: &str, name: &str, age: Duration| {
            let path = dir.join(folder).join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            File::create(&path)
                .unwrap()
                .set_modified(SystemTime::now() - age)
                .unwrap();
            path
        };
        let old_grant = file("grants", "a.json", GRANT_LIFETIME + Duration::from_secs(1));
        let new_grant = file("grants", "b.json", Duration::from_secs(1));
        let old_request = file(
            "requests",
            "c.json",
            REQUEST_LIFETIME + Duration::from_secs(1),
        );
        let new_request = file("requests", "d.json", GRANT_LIFETIME * 2);
        sweep(&dir);
        assert!(!old_grant.exists());
        assert!(new_grant.exists());
        assert!(!old_request.exists());
        assert!(new_request.exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}
