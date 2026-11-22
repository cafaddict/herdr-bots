//! Registers the daemon with the OS service manager: launchd on macOS, systemd --user on Linux.
//! Both restart it if it dies and start it at login.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};

use crate::paths::Paths;

const LABEL: &str = "dev.herdr-bots";

pub fn install(paths: &Paths) -> anyhow::Result<PathBuf> {
    paths.ensure()?;
    let exe = std::env::current_exe()?;
    // The daemon starts agents, gh, git and hc, so it needs the PATH of the shell that installed it.
    let path_env = std::env::var("PATH").unwrap_or_default();
    if cfg!(target_os = "macos") {
        install_launchd(paths, &exe, &path_env)
    } else if cfg!(target_os = "linux") {
        install_systemd(paths, &exe, &path_env)
    } else {
        bail!("only macOS (launchd) and Linux (systemd --user) are supported")
    }
}

pub fn uninstall() -> anyhow::Result<()> {
    if cfg!(target_os = "macos") {
        let plist = launch_agents()?.join(format!("{LABEL}.plist"));
        let _ = Command::new("launchctl")
            .args(["bootout", &gui_domain()?, &plist.to_string_lossy()])
            .status();
        let _ = std::fs::remove_file(plist);
    } else {
        let _ = Command::new("systemctl")
            .args(["--user", "disable", "--now", "herdr-bots.service"])
            .status();
        let _ = std::fs::remove_file(systemd_dir()?.join("herdr-bots.service"));
        let _ = Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .status();
    }
    Ok(())
}

fn launch_agents() -> anyhow::Result<PathBuf> {
    Ok(home()?.join("Library/LaunchAgents"))
}

fn systemd_dir() -> anyhow::Result<PathBuf> {
    Ok(home()?.join(".config/systemd/user"))
}

fn home() -> anyhow::Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")
}

fn gui_domain() -> anyhow::Result<String> {
    let out = Command::new("id").arg("-u").output()?;
    Ok(format!(
        "gui/{}",
        String::from_utf8_lossy(&out.stdout).trim()
    ))
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub fn launchd_plist(exe: &Path, root: &Path, logs: &Path, path_env: &str) -> String {
    let e = |p: &Path| xml_escape(&p.to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array><string>{exe}</string><string>daemon</string></array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key><string>{path}</string>
    <key>HERDR_BOTS_HOME</key><string>{root}</string>
  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ThrottleInterval</key><integer>10</integer>
  <key>StandardOutPath</key><string>{logs}/daemon.log</string>
  <key>StandardErrorPath</key><string>{logs}/daemon.log</string>
</dict>
</plist>
"#,
        exe = e(exe),
        path = xml_escape(path_env),
        root = e(root),
        logs = e(logs),
    )
}

pub fn systemd_unit(exe: &Path, root: &Path, path_env: &str) -> String {
    format!(
        "[Unit]\nDescription=herdr-bots daemon\n\n[Service]\nExecStart={exe} daemon\nEnvironment=\"PATH={path_env}\"\nEnvironment=\"HERDR_BOTS_HOME={root}\"\nRestart=always\nRestartSec=10\n\n[Install]\nWantedBy=default.target\n",
        exe = exe.display(),
        root = root.display(),
    )
}

fn install_launchd(paths: &Paths, exe: &Path, path_env: &str) -> anyhow::Result<PathBuf> {
    let dir = launch_agents()?;
    std::fs::create_dir_all(&dir)?;
    let plist = dir.join(format!("{LABEL}.plist"));
    std::fs::write(
        &plist,
        launchd_plist(exe, paths.root(), &paths.logs_dir(), path_env),
    )?;
    let domain = gui_domain()?;
    let target = plist.to_string_lossy().into_owned();
    // Replace a running copy (bootout fails harmlessly when none is loaded).
    let _ = Command::new("launchctl")
        .args(["bootout", &domain, &target])
        .status();
    let ok = Command::new("launchctl")
        .args(["bootstrap", &domain, &target])
        .status()?
        .success();
    if !ok {
        bail!("launchctl bootstrap {domain} {target} failed");
    }
    Ok(plist)
}

fn install_systemd(paths: &Paths, exe: &Path, path_env: &str) -> anyhow::Result<PathBuf> {
    let dir = systemd_dir()?;
    std::fs::create_dir_all(&dir)?;
    let unit = dir.join("herdr-bots.service");
    std::fs::write(&unit, systemd_unit(exe, paths.root(), path_env))?;
    for args in [
        &["--user", "daemon-reload"][..],
        &["--user", "enable", "--now", "herdr-bots.service"],
    ] {
        if !Command::new("systemctl").args(args).status()?.success() {
            bail!("systemctl {} failed", args.join(" "));
        }
    }
    let _ = Command::new("systemctl")
        .args(["--user", "restart", "herdr-bots.service"])
        .status();
    Ok(unit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_escapes_and_names_the_daemon() {
        let p = launchd_plist(
            Path::new("/a&b/herdr-bots"),
            Path::new("/r"),
            Path::new("/r/logs"),
            "/bin:/usr/bin",
        );
        assert!(p.contains("<string>/a&amp;b/herdr-bots</string><string>daemon</string>"));
        assert!(p.contains("<key>KeepAlive</key><true/>"));
    }

    #[test]
    fn unit_restarts() {
        let u = systemd_unit(Path::new("/x"), Path::new("/r"), "/bin");
        assert!(u.contains("ExecStart=/x daemon") && u.contains("Restart=always"));
    }
}
