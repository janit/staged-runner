//! A service definition that keeps `staged-runner run` going: a systemd unit
//! on Linux, a launchd daemon on macOS. Both start at boot.

use std::path::Path;

/// `user` runs the service; None runs it as root. The actions runner refuses
/// to run as root, so native runners need a user.
pub fn render(exe: &Path, config: &Path, host: &str, user: Option<&str>) -> Result<String, String> {
    // Neither format is escaped here, so refuse what would need it.
    for word in [
        &*exe.to_string_lossy(),
        &*config.to_string_lossy(),
        user.unwrap_or(""),
    ] {
        if word
            .chars()
            .any(|c| c.is_whitespace() || "%&<>\"'\\$;".contains(c))
        {
            return Err(format!(
                "{word:?}: use a path or name without spaces or special characters"
            ));
        }
    }
    Ok(if cfg!(target_os = "macos") {
        launchd(exe, config, host, user)
    } else {
        systemd(exe, config, host, user)
    })
}

fn systemd(exe: &Path, config: &Path, host: &str, user: Option<&str>) -> String {
    let user = user.map(|u| format!("User={u}\n")).unwrap_or_default();
    format!(
        "# /etc/systemd/system/staged-runner.service
[Unit]
Description=staged-runner for host {host}
After=network-online.target docker.service
Wants=network-online.target

[Service]
{user}ExecStart={} --config {} run {host}
Restart=always
RestartSec=10
# Stopping the service leaves the runners as they are.
KillMode=process

[Install]
WantedBy=multi-user.target
",
        exe.display(),
        config.display()
    )
}

fn launchd(exe: &Path, config: &Path, host: &str, user: Option<&str>) -> String {
    // A user cannot write to /var/log; its own Logs folder is the macOS place.
    let log = user.map_or("/var/log/staged-runner.log".to_string(), |u| {
        format!("/Users/{u}/Library/Logs/staged-runner.log")
    });
    let user = user
        .map(|u| format!("  <key>UserName</key><string>{u}</string>\n"))
        .unwrap_or_default();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!-- /Library/LaunchDaemons/io.github.staged-runner.plist -->
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>io.github.staged-runner</string>
{user}  <key>ProgramArguments</key>
  <array>
    <string>{}</string>
    <string>--config</string><string>{}</string>
    <string>run</string><string>{host}</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict><key>PATH</key><string>/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin</string></dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>AbandonProcessGroup</key><true/>
  <key>StandardErrorPath</key><string>{log}</string>
</dict>
</plist>
"#,
        exe.display(),
        config.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_definitions_run_the_host_as_the_user() {
        let (exe, config) = (
            Path::new("/usr/local/bin/staged-runner"),
            Path::new("/etc/f.toml"),
        );
        let unit = systemd(exe, config, "A", Some("ci"));
        assert!(unit.contains("ExecStart=/usr/local/bin/staged-runner --config /etc/f.toml run A"));
        assert!(unit.contains("User=ci\n"));
        assert!(!systemd(exe, config, "A", None).contains("User="));
        let plist = launchd(exe, config, "A", Some("ci"));
        assert!(plist.contains("<string>run</string><string>A</string>"));
        assert!(plist.contains("<key>UserName</key><string>ci</string>"));
        assert!(plist.contains("/Users/ci/Library/Logs/staged-runner.log"));
        assert!(render(Path::new("/my dir/x"), config, "A", None).is_err());
        assert!(render(exe, Path::new("/etc/a&b.toml"), "A", None).is_err());
        assert!(render(exe, config, "A", Some("ci")).is_ok());
    }
}
