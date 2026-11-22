//! GitHub through the `gh` CLI (it already holds the user's auth).

use anyhow::{Context, bail};
use serde::Deserialize;
use tokio::process::Command;

use super::Found;
use crate::config::{GithubConfig, GithubEvent};

#[derive(Debug, Deserialize)]
struct Login {
    login: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Repo {
    name_with_owner: String,
}

/// One row of `gh search prs|issues --json ...`.
#[derive(Debug, Deserialize)]
struct SearchHit {
    number: u64,
    title: String,
    url: String,
    #[serde(default)]
    body: String,
    author: Login,
    repository: Repo,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrChecks {
    head_ref_oid: String,
    #[serde(default)]
    status_check_rollup: Vec<Check>,
}

#[derive(Debug, Deserialize)]
struct Check {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    context: Option<String>,
    /// CheckRun
    #[serde(default)]
    conclusion: Option<String>,
    /// StatusContext
    #[serde(default)]
    state: Option<String>,
}

impl Check {
    fn failed(&self) -> bool {
        let bad = |s: &Option<String>| {
            s.as_deref()
                .is_some_and(|v| matches!(v, "FAILURE" | "TIMED_OUT" | "ERROR" | "STARTUP_FAILURE"))
        };
        bad(&self.conclusion) || bad(&self.state)
    }

    fn label(&self) -> &str {
        self.name
            .as_deref()
            .or(self.context.as_deref())
            .unwrap_or("check")
    }
}

const SEARCH_FIELDS: &str = "number,title,url,body,author,repository";

pub async fn whoami(gh: &str) -> anyhow::Result<String> {
    Ok(run(gh, &["api", "user", "--jq", ".login"])
        .await?
        .trim()
        .to_owned())
}

pub async fn poll(gh: &str, cfg: &GithubConfig) -> anyhow::Result<Vec<Found>> {
    let mut found = Vec::new();
    for event in &cfg.events {
        match event {
            GithubEvent::ReviewRequested => {
                for hit in search(gh, "prs", "--review-requested=@me", &cfg.repos).await? {
                    found.push(hit_to_found(hit, "review", "Review requested"));
                }
            }
            GithubEvent::Assigned => {
                for hit in search(gh, "issues", "--assignee=@me", &cfg.repos).await? {
                    found.push(hit_to_found(hit, "assigned", "Assigned to you"));
                }
            }
            GithubEvent::CiFailed => {
                for hit in search(gh, "prs", "--author=@me", &cfg.repos).await? {
                    if let Some(f) = ci_failure(gh, hit).await? {
                        found.push(f);
                    }
                }
            }
        }
    }
    Ok(found)
}

async fn search(
    gh: &str,
    kind: &str,
    filter: &str,
    repos: &[String],
) -> anyhow::Result<Vec<SearchHit>> {
    let mut args = vec![
        "search",
        kind,
        filter,
        "--state=open",
        "--limit=50",
        "--json",
        SEARCH_FIELDS,
    ];
    for repo in repos {
        args.extend(["--repo", repo.as_str()]);
    }
    let out = run(gh, &args).await?;
    parse_hits(&out)
}

fn parse_hits(json: &str) -> anyhow::Result<Vec<SearchHit>> {
    serde_json::from_str(json).context("gh search output")
}

fn hit_to_found(hit: SearchHit, kind: &str, what: &str) -> Found {
    let repo = hit.repository.name_with_owner;
    Found {
        key: format!("{kind}:{repo}#{}", hit.number),
        source: "github",
        title: format!("{what}: {repo}#{} {}", hit.number, hit.title),
        body: hit.body,
        url: hit.url,
        author: hit.author.login,
    }
}

/// A failed check on the PR's current head; keyed by head SHA so a new push can fail again.
async fn ci_failure(gh: &str, hit: SearchHit) -> anyhow::Result<Option<Found>> {
    let repo = hit.repository.name_with_owner.clone();
    let number = hit.number.to_string();
    let out = run(
        gh,
        &[
            "pr",
            "view",
            &number,
            "-R",
            &repo,
            "--json",
            "headRefOid,statusCheckRollup",
        ],
    )
    .await?;
    let checks: PrChecks = serde_json::from_str(&out).context("gh pr view output")?;
    let failed: Vec<&str> = checks
        .status_check_rollup
        .iter()
        .filter(|c| c.failed())
        .map(Check::label)
        .collect();
    if failed.is_empty() {
        return Ok(None);
    }
    let sha = &checks.head_ref_oid[..checks.head_ref_oid.len().min(12)];
    Ok(Some(Found {
        key: format!("ci:{repo}#{}@{sha}", hit.number),
        source: "github",
        title: format!("CI failed: {repo}#{} {}", hit.number, hit.title),
        body: format!("Failed checks on {sha}: {}", failed.join(", ")),
        url: hit.url,
        author: hit.author.login,
    }))
}

async fn run(gh: &str, args: &[&str]) -> anyhow::Result<String> {
    let out = Command::new(gh)
        .args(args)
        .output()
        .await
        .with_context(|| format!("could not run {gh}"))?;
    if !out.status.success() {
        bail!(
            "gh {}: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HITS: &str = r#"[{"number":12,"title":"Fix login","url":"https://github.com/o/r/pull/12",
        "body":"please","author":{"login":"alice","type":"User"},
        "repository":{"name":"r","nameWithOwner":"o/r"}}]"#;

    #[test]
    fn search_hit_becomes_found() {
        let hit = parse_hits(HITS).unwrap().into_iter().next().unwrap();
        let f = hit_to_found(hit, "review", "Review requested");
        assert_eq!(f.key, "review:o/r#12");
        assert_eq!(f.author, "alice");
        assert_eq!(f.title, "Review requested: o/r#12 Fix login");
    }

    #[test]
    fn check_failure_detection() {
        let checks: PrChecks = serde_json::from_str(
            r#"{"headRefOid":"abc","statusCheckRollup":[
                {"__typename":"CheckRun","name":"build","conclusion":"SUCCESS"},
                {"__typename":"CheckRun","name":"test","conclusion":"FAILURE"},
                {"__typename":"StatusContext","context":"ci/lint","state":"ERROR"},
                {"__typename":"CheckRun","name":"slow","conclusion":""}]}"#,
        )
        .unwrap();
        let failed: Vec<&str> = checks
            .status_check_rollup
            .iter()
            .filter(|c| c.failed())
            .map(Check::label)
            .collect();
        assert_eq!(failed, ["test", "ci/lint"]);
    }
}
