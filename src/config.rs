//! The fleet file: every host in spill-over order. The same file goes on every host.

use serde::Deserialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Address the status endpoint listens on.
    #[serde(default = "default_listen")]
    pub listen: String,
    /// Seconds between checks.
    #[serde(default = "default_poll")]
    pub poll: u64,
    #[serde(rename = "host", default)]
    pub hosts: Vec<Host>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Host {
    pub name: String,
    /// Base URL of the host's status endpoint, e.g. http://10.0.0.11:7391
    pub endpoint: String,
    pub runners: u32,
    /// Overrides the top-level listen for this host, e.g. its VPN address.
    pub listen: Option<String>,
    pub kind: Kind,
    /// For kind = "native": the directory holding runner-<host>-<n>/.
    pub root: Option<PathBuf>,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// A Docker container named runner-<host>-<n>.
    Docker,
    /// An actions-runner directory <root>/runner-<host>-<n>, started with run.sh.
    Native,
}

const MAX_RUNNERS: u32 = 64;

fn default_listen() -> String {
    "0.0.0.0:7391".into()
}

fn default_poll() -> u64 {
    10
}

pub fn runner_name(host: &str, n: u32) -> String {
    format!("runner-{host}-{n}")
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Config::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Config, String> {
        let config: Config = toml::from_str(text).map_err(|e| e.to_string())?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), String> {
        if self.hosts.is_empty() {
            return Err("no [[host]] entries".into());
        }
        if self.poll == 0 {
            return Err("poll must be at least 1".into());
        }
        let mut seen = HashSet::new();
        for h in &self.hosts {
            let ok = !h.name.is_empty()
                && h.name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.');
            if !ok {
                return Err(format!(
                    "host name {:?}: use letters, digits, - _ .",
                    h.name
                ));
            }
            // GitHub runner names ignore case.
            if !seen.insert(h.name.to_lowercase()) {
                return Err(format!("host {} is listed twice", h.name));
            }
            if !(1..=MAX_RUNNERS).contains(&h.runners) {
                return Err(format!(
                    "host {}: runners must be 1 to {MAX_RUNNERS}",
                    h.name
                ));
            }
            let address = h.endpoint.strip_prefix("http://").unwrap_or("");
            if address.is_empty() || address.starts_with('/') {
                return Err(format!(
                    "host {}: endpoint must be http://address:port",
                    h.name
                ));
            }
            match (h.kind, &h.root) {
                (Kind::Native, None) => {
                    return Err(format!("host {}: kind = \"native\" needs root", h.name));
                }
                (Kind::Native, Some(root)) if !root.is_absolute() => {
                    return Err(format!("host {}: root must be an absolute path", h.name));
                }
                (Kind::Docker, Some(_)) => {
                    return Err(format!(
                        "host {}: root is only for kind = \"native\"",
                        h.name
                    ));
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub fn listen<'a>(&'a self, host: &'a Host) -> &'a str {
        host.listen.as_deref().unwrap_or(&self.listen)
    }

    pub fn host(&self, name: &str) -> Result<&Host, String> {
        self.hosts
            .iter()
            .find(|h| h.name == name)
            .ok_or_else(|| format!("no host {name} in the fleet file"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FLEET: &str = r#"
        [[host]]
        name = "A"
        endpoint = "http://a:7391"
        runners = 2
        kind = "docker"

        [[host]]
        name = "B"
        endpoint = "http://b:7391"
        runners = 1
        listen = "100.64.0.2:7391"
        kind = "native"
        root = "/opt/runners"
    "#;

    #[test]
    fn parses_a_fleet_with_defaults() {
        let c = Config::parse(FLEET).unwrap();
        assert_eq!(c.listen, "0.0.0.0:7391");
        assert_eq!(c.poll, 10);
        assert_eq!(c.hosts.len(), 2);
        assert_eq!(c.hosts[1].kind, Kind::Native);
        assert_eq!(c.host("B").unwrap().runners, 1);
        assert_eq!(c.listen(&c.hosts[0]), "0.0.0.0:7391");
        assert_eq!(c.listen(&c.hosts[1]), "100.64.0.2:7391");
        assert!(c.host("C").is_err());
    }

    #[test]
    fn refuses_a_bad_fleet() {
        let cases = [
            ("", "no [[host]]"),
            (&FLEET.replace("\"B\"", "\"A\""), "listed twice"),
            (&FLEET.replace("runners = 1", "runners = 0"), "1 to 64"),
            (
                &FLEET.replace("runners = 1", "runners = 4000000000"),
                "1 to 64",
            ),
            (&FLEET.replace("\"B\"", "\"a\""), "listed twice"),
            (&FLEET.replace("http://b:7391", "http://"), "address:port"),
            (&FLEET.replace("/opt/runners", "runners"), "absolute"),
            (
                &FLEET.replace("kind = \"docker\"", "kind = \"docker\"\nroot = \"/x\""),
                "only for",
            ),
            (&FLEET.replace("root = \"/opt/runners\"", ""), "needs root"),
            (&FLEET.replace("\"A\"", "\"a b\""), "use letters"),
            (&FLEET.replace("http://a", "https://a"), "address:port"),
            (
                &FLEET.replace("kind = \"docker\"", "kind = \"vm\""),
                "unknown variant",
            ),
            (&format!("polll = 3\n{FLEET}"), "unknown field"),
        ];
        for (text, want) in cases {
            let err = Config::parse(text).unwrap_err();
            assert!(err.contains(want), "{err:?} should say {want:?}");
        }
    }
}
