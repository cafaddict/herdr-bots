//! Linear issues assigned to me, through its GraphQL API and `curl`.
//!
//! `curl` instead of an HTTP crate keeps the binary small and TLS on the system's stack. The API
//! key goes in on stdin (`-H @-`), never on the command line where `ps` would show it.

use std::process::Stdio;

use anyhow::{Context, bail};
use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use super::Found;
use crate::config::LinearConfig;
use crate::paths::Paths;

const ENDPOINT: &str = "https://api.linear.app/graphql";
const KEYCHAIN_SERVICE: &str = "herdr-bots-linear";

const QUERY: &str = r#"{"query":"{ viewer { assignedIssues(first: 50, filter: { state: { type: { nin: [\"completed\", \"canceled\"] } } }) { nodes { identifier title url description team { key } creator { email } } } } }"}"#;

#[derive(Debug, Deserialize)]
struct Response {
    data: Option<Data>,
    #[serde(default)]
    errors: Vec<serde_json::Value>,
}
#[derive(Debug, Deserialize)]
struct Data {
    viewer: Viewer,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Viewer {
    assigned_issues: Nodes,
}
#[derive(Debug, Deserialize)]
struct Nodes {
    nodes: Vec<Issue>,
}
#[derive(Debug, Deserialize)]
struct Issue {
    identifier: String,
    title: String,
    url: String,
    description: Option<String>,
    team: Team,
    creator: Option<Creator>,
}
#[derive(Debug, Deserialize)]
struct Team {
    key: String,
}
#[derive(Debug, Deserialize)]
struct Creator {
    email: Option<String>,
}

/// `$LINEAR_API_KEY`, else the macOS Keychain item `herdr-bots-linear`, else `secrets/linear`.
pub async fn api_key(paths: &Paths) -> anyhow::Result<String> {
    if let Ok(key) = std::env::var("LINEAR_API_KEY")
        && !key.trim().is_empty()
    {
        return Ok(key.trim().to_owned());
    }
    if cfg!(target_os = "macos") {
        let out = Command::new("security")
            .args(["find-generic-password", "-s", KEYCHAIN_SERVICE, "-w"])
            .output()
            .await;
        if let Ok(out) = out
            && out.status.success()
        {
            return Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned());
        }
    }
    let file = paths.secrets_dir().join("linear");
    let key = std::fs::read_to_string(&file).with_context(|| {
        format!(
            "no Linear API key: set LINEAR_API_KEY, the Keychain item `{KEYCHAIN_SERVICE}`, or {}",
            file.display()
        )
    })?;
    Ok(key.trim().to_owned())
}

pub async fn poll(paths: &Paths, cfg: &LinearConfig) -> anyhow::Result<Vec<Found>> {
    let key = api_key(paths).await?;
    let mut child = Command::new("curl")
        .args([
            "-sS",
            "--fail-with-body",
            "-X",
            "POST",
            ENDPOINT,
            "-H",
            "Content-Type: application/json",
            "-H",
            "@-",
        ])
        .arg("--data")
        .arg(QUERY)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("could not run curl")?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(format!("Authorization: {key}\n").as_bytes())
            .await?;
    }
    let out = child.wait_with_output().await?;
    if !out.status.success() {
        bail!("linear: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    parse(&String::from_utf8_lossy(&out.stdout), cfg)
}

fn parse(json: &str, cfg: &LinearConfig) -> anyhow::Result<Vec<Found>> {
    let resp: Response = serde_json::from_str(json).context("linear response")?;
    if !resp.errors.is_empty() {
        bail!("linear: {}", serde_json::Value::Array(resp.errors));
    }
    let issues = resp
        .data
        .context("linear: no data")?
        .viewer
        .assigned_issues
        .nodes;
    Ok(issues
        .into_iter()
        .filter(|i| {
            cfg.team
                .as_ref()
                .is_none_or(|t| t.eq_ignore_ascii_case(&i.team.key))
        })
        .map(|i| Found {
            key: format!("linear:{}", i.identifier),
            source: "linear",
            title: format!("Linear {}: {}", i.identifier, i.title),
            body: i.description.unwrap_or_default(),
            url: i.url,
            author: i.creator.and_then(|c| c.email).unwrap_or_default(),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const RESP: &str = r#"{"data":{"viewer":{"assignedIssues":{"nodes":[
        {"identifier":"ENG-1","title":"A","url":"https://linear.app/x/issue/ENG-1","description":"d",
         "team":{"key":"ENG"},"creator":{"email":"a@x.dev"}},
        {"identifier":"OPS-2","title":"B","url":"u","description":null,"team":{"key":"OPS"},"creator":null}]}}}}"#;

    #[test]
    fn filters_by_team() {
        let all = parse(RESP, &LinearConfig { team: None }).unwrap();
        assert_eq!(all.len(), 2);
        let eng = parse(
            RESP,
            &LinearConfig {
                team: Some("eng".into()),
            },
        )
        .unwrap();
        assert_eq!(eng.len(), 1);
        assert_eq!(eng[0].key, "linear:ENG-1");
        assert_eq!(eng[0].author, "a@x.dev");
    }

    #[test]
    fn graphql_errors_are_errors() {
        assert!(
            parse(
                r#"{"data":null,"errors":[{"message":"auth"}]}"#,
                &LinearConfig { team: None }
            )
            .is_err()
        );
    }

    #[test]
    fn query_is_valid_json() {
        let v: serde_json::Value = serde_json::from_str(QUERY).unwrap();
        assert!(v["query"].as_str().unwrap().contains("assignedIssues"));
    }
}
