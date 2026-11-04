//! Local installed-instance discovery. Never changes a daemon's target or forwards environment credentials.
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::{io::{Read, Write}, path::Path, process::{Command, Stdio}};
use crate::client::DaemonClient;
const LABEL: &str = "com.leeguoo.iphone-use";

pub(crate) fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase()) && name.len() <= 32
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !["runner", "wda", "autoupdate", "daily-maintenance", "flow-reverify"].contains(&name)
}

fn read_private_plist(path: &Path) -> Result<Value> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let mut file = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW)
        .open(path).context("installed instance configuration is unavailable")?;
    let metadata = file.metadata()?;
    anyhow::ensure!(metadata.is_file() && metadata.uid() == unsafe { libc::geteuid() }
        && metadata.mode() & 0o077 == 0 && metadata.len() <= 262144,
        "instance configuration must be a private regular file owned by this user");
    let mut bytes = Vec::new();
    Read::by_ref(&mut file).take(262145).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= 262144, "instance configuration is too large");
    let mut child = Command::new("/usr/bin/plutil").args(["-convert", "json", "-o", "-", "-"])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn()
        .context("cannot read installed instance configuration on this host")?;
    let written = child.stdin.take().context("configuration reader stdin unavailable")?.write_all(&bytes);
    let output = child.wait_with_output()?;
    anyhow::ensure!(written.is_ok() && output.status.success(), "invalid installed instance configuration");
    serde_json::from_slice(&output.stdout).map_err(|_| anyhow::anyhow!("invalid installed instance configuration"))
}

fn parse_target(name: &str, value: &Value) -> Result<(String, Option<String>)> {
    anyhow::ensure!(valid_name(name), "invalid instance name");
    let label = if name == "default" { LABEL.to_string() } else { format!("{LABEL}.{name}") };
    anyhow::ensure!(value["Label"].as_str() == Some(&label), "instance LaunchAgent label does not match");
    let env = &value["EnvironmentVariables"];
    let port = env["IPHONE_USE_PORT"].as_str().and_then(|s| s.parse::<u16>().ok())
        .filter(|p| *p > 0).context("installed instance has no valid daemon port")?;
    let token = ["IPHONE_USE_AGENT_TOKEN", "IPHONE_USE_PASSWORD"].iter()
        .find_map(|key| env[*key].as_str().filter(|s| !s.is_empty()).map(str::to_owned));
    // The address is constructed locally. Never use a plist URL or ambient token.
    Ok((format!("http://127.0.0.1:{port}"), token))
}

pub(crate) fn client_at(home: &Path, name: &str, owner: &str) -> Result<DaemonClient> {
    anyhow::ensure!(valid_name(name), "invalid instance name");
    let label = if name == "default" { LABEL.to_string() } else { format!("{LABEL}.{name}") };
    let value = read_private_plist(&home.join("Library/LaunchAgents").join(format!("{label}.plist")))?;
    let (url, token) = parse_target(name, &value)?;
    Ok(DaemonClient::new(url, token).with_owner(owner))
}

pub(crate) fn client(name: &str, owner: &str) -> Result<DaemonClient> {
    let home = std::env::var_os("HOME").context("HOME is not configured")?;
    client_at(Path::new(&home), name, owner)
}

pub(crate) fn names_at(home: &Path) -> Result<Vec<String>> {
    let dir = home.join("Library/LaunchAgents");
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry?;
        let file = entry.file_name();
        let Some(file) = file.to_str().and_then(|s| s.strip_suffix(".plist")) else { continue; };
        let name = if file == LABEL { "default" } else if let Some(name) = file.strip_prefix(&format!("{LABEL}.")) { name } else { continue; };
        if valid_name(name) {
            names.push(name.to_string());
            if names.len() > 128 { bail!("too many installed instances to enumerate"); }
        }
    }
    names.sort(); names.dedup(); Ok(names)
}

pub(crate) fn names() -> Result<Vec<String>> {
    let home = std::env::var_os("HOME").context("HOME is not configured")?;
    names_at(Path::new(&home))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};
    #[test]
    fn names_match_the_shared_instance_contract() {
        let fixture: Value = serde_json::from_str(include_str!("../../../scripts/fixtures/instance-derivation.json")).unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            assert!(name.is_empty() || valid_name(name));
        }
        for name in fixture["invalid_names"].as_array().unwrap() { assert!(!valid_name(name.as_str().unwrap())); }
    }
    #[test]
    fn target_uses_only_its_label_port_and_preferred_credentials() {
        let value = serde_json::json!({"Label":"com.leeguoo.iphone-use.i13","EnvironmentVariables":{
            "IPHONE_USE_PORT":"45838","IPHONE_USE_AGENT_TOKEN":"agent-secret","IPHONE_USE_PASSWORD":"password-secret",
            "IPHONE_USE_URL":"https://untrusted.example"}});
        let (url, token) = parse_target("i13", &value).unwrap();
        assert_eq!(url,"http://127.0.0.1:45838"); assert_eq!(token.as_deref(),Some("agent-secret"));
        assert!(parse_target("default", &value).is_err());
        assert!(parse_target("../i13", &value).is_err());
        let mut invalid = value;
        for port in ["0", "65536", "-1", "https://remote.example"] {
            invalid["EnvironmentVariables"]["IPHONE_USE_PORT"] = port.into();
            assert!(parse_target("i13", &invalid).is_err());
        }
    }
    #[test]
    fn discovery_does_not_turn_path_errors_into_an_empty_inventory() {
        let dir = tempfile::tempdir().unwrap();
        assert!(names_at(dir.path()).unwrap().is_empty());
        std::fs::create_dir(dir.path().join("Library")).unwrap();
        symlink("LaunchAgents", dir.path().join("Library/LaunchAgents")).unwrap();
        assert!(names_at(dir.path()).is_err(), "a symlink loop must remain a discovery error");
    }

    #[test]
    fn discovery_ignores_supervisors_and_private_reader_rejects_unsafe_paths() {
        let dir = tempfile::tempdir().unwrap();
        let agents = dir.path().join("Library/LaunchAgents"); std::fs::create_dir_all(&agents).unwrap();
        let file = agents.join("com.leeguoo.iphone-use.i13.plist");
        std::fs::write(&file, "not a plist").unwrap();
        std::fs::write(agents.join("com.leeguoo.iphone-use.runner.i13.plist"), "").unwrap();
        assert_eq!(names_at(dir.path()).unwrap(),vec!["i13"]);
        assert!(read_private_plist(&file).is_err());
        std::fs::set_permissions(&file,std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(read_private_plist(&file).is_err());
        let link = agents.join("com.leeguoo.iphone-use.default.plist"); symlink(&file,&link).unwrap();
        assert!(read_private_plist(&link).is_err());
        assert!(client_at(dir.path(), "missing", "test-owner").is_err());
    }
}
