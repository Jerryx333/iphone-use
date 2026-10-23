//! Process, file and launchd plumbing for setup. Every external tool setup
//! still calls (xcodebuild, xcrun, devicectl, launchctl, lsof, plutil, ps)
//! goes through here, so the rules live in one place: a bounded wait for
//! anything that can hang, `LC_ALL=C` wherever output is compared, and no
//! shell in between.

use std::io::{Read as _, Write as _};
use std::os::fd::AsRawFd as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use crate::usbmux::Value;

/// The PATH every setup child sees. A LaunchAgent starts with a bare PATH;
/// Homebrew tools (socat, a legacy iproxy) and xcrun helpers live outside it.
pub fn extend_path() {
    let current = std::env::var("PATH").unwrap_or_default();
    let extended = if current.is_empty() {
        "/opt/homebrew/bin:/usr/local/bin:/usr/sbin:/sbin:/usr/bin:/bin".to_string()
    } else {
        format!("/opt/homebrew/bin:/usr/local/bin:/usr/sbin:/sbin:{current}")
    };
    // Called once at startup, before any thread exists.
    std::env::set_var("PATH", extended);
}

pub fn uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}

/// The full path of `name` on PATH, like `command -v`.
pub fn which(name: &str) -> Option<PathBuf> {
    if name.contains('/') {
        let path = PathBuf::from(name);
        return is_executable(&path).then_some(path);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

pub fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// Run to completion, stdin closed. `None` when it could not start.
pub fn run(program: &str, args: &[&str]) -> Option<Output> {
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .ok()
}

/// Stdout of a successful run, trimmed. Empty on any failure.
pub fn stdout_of(program: &str, args: &[&str]) -> String {
    run(program, args)
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .unwrap_or_default()
}

/// Run with a deadline; the command's process group is killed on timeout. Returns its
/// stdout (whatever it printed before the kill) and whether it exited 0.
///
/// devicectl can hang forever on a device whose tunnel is stuck connecting
/// (hardware-verified 2026-06-12); every devicectl call goes through here.
pub fn run_bounded(program: &str, args: &[&str], limit: Duration) -> (String, bool) {
    let Ok(mut child) = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
    else {
        return (String::new(), false);
    };
    let mut stdout = child.stdout.take().expect("piped command stdout");
    let fd = stdout.as_raw_fd();
    // SAFETY: stdout owns this live descriptor; preserve its existing flags.
    let nonblocking = unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        flags >= 0 && libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) == 0
    };
    if !nonblocking {
        kill_command_group(&mut child);
        return (String::new(), false);
    }
    // A wrapper's descendant may inherit stdout after the wrapper exits or
    // is killed. Never wait for pipe EOF (or join a reader waiting for it).
    let mut text = Vec::new();
    let mut buffer = [0; 8192];
    let deadline = Instant::now() + limit;
    let mut exited = false;
    loop {
        if !exited {
            match command_exit_pending(&child) {
                Ok(status) => exited = status,
                Err(_) => {
                    kill_command_group(&mut child);
                    return (String::from_utf8_lossy(&text).into_owned(), false);
                }
            }
        }
        // Limit each drain so a continuously writing command cannot starve
        // the deadline check. If the parent exited, drain buffered bytes only.
        let mut drained = false;
        for _ in 0..8 {
            match stdout.read(&mut buffer) {
                Ok(0) => {
                    drained = true;
                    break;
                }
                Ok(n) => text.extend_from_slice(&buffer[..n]),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    drained = true;
                    break;
                }
                Err(_) => {
                    kill_command_group(&mut child);
                    return (String::from_utf8_lossy(&text).into_owned(), false);
                }
            }
        }
        if exited && drained {
            let success = child.wait().is_ok_and(|status| status.success());
            return (String::from_utf8_lossy(&text).into_owned(), success);
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            kill_command_group(&mut child);
            return (String::from_utf8_lossy(&text).into_owned(), false);
        }
        if drained {
            std::thread::sleep(left.min(Duration::from_millis(10)));
        }
    }
}

/// Check for exit without reaping. The leader's PID must remain reserved
/// until group cleanup or the successful buffered-output drain is complete.
fn command_exit_pending(child: &std::process::Child) -> std::io::Result<bool> {
    // SAFETY: a zeroed siginfo_t is valid storage for waitid. With WNOHANG,
    // a zero PID also represents no available exit event.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    loop {
        // SAFETY: P_PID restricts this to our child; WNOWAIT leaves the exit
        // status waitable and prevents this check from releasing its PID.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                child.id() as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result == 0 {
            // SAFETY: waitid initialized the exit event, or left our zero PID.
            return Ok(unsafe { info.si_pid() } == child.id() as libc::pid_t);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

fn kill_command_group(child: &mut std::process::Child) {
    // A successful non-reaping check confirms the leader is still our child,
    // even if it exited. process_group(0) isolates it from the caller's group.
    // Never signal a numeric group after ownership has been lost (ECHILD).
    if command_exit_pending(child).is_ok() {
        if let Ok(group) = i32::try_from(child.id()) {
            // SAFETY: this is the dedicated process group of our live child.
            unsafe {
                libc::kill(-group, libc::SIGKILL);
            }
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// `xcrun devicectl <args>` bounded to `secs`.
pub fn devicectl(secs: u64, args: &[&str]) -> String {
    let mut full = vec!["devicectl"];
    full.extend_from_slice(args);
    run_bounded("xcrun", &full, Duration::from_secs(secs)).0
}

/// `devicectl … -j <file>` bounded to `secs`, returning the JSON file's text.
pub fn devicectl_json(secs: u64, args: &[&str]) -> Option<String> {
    let file = tempfile::Builder::new()
        .prefix("iphone-use-devicectl.")
        .tempfile()
        .ok()?;
    let path = file.path().to_string_lossy().into_owned();
    let mut full: Vec<&str> = args.to_vec();
    full.extend_from_slice(&["-j", &path]);
    devicectl(secs, &full);
    std::fs::read_to_string(file.path())
        .ok()
        .filter(|text| !text.trim().is_empty())
}

/// One `ps` column for `pid`, `LC_ALL=C`, trimmed; empty when gone.
fn ps_column(pid: u32, column: &str, wide: bool) -> String {
    let pid = pid.to_string();
    let mut args = vec!["-p", pid.as_str(), "-o", column];
    if wide {
        args.insert(0, "-ww");
    }
    Command::new("ps")
        .env("LC_ALL", "C")
        .args(&args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .unwrap_or_default()
}

/// The process start time exactly as setup has always recorded it
/// (`LC_ALL=C ps -p PID -o lstart=`, trimmed); the daemon compares it.
pub fn ps_lstart(pid: u32) -> String {
    ps_column(pid, "lstart=", false)
}

pub fn ps_uid(pid: u32) -> Option<u32> {
    ps_column(pid, "uid=", false).parse().ok()
}

pub fn ps_command(pid: u32) -> String {
    ps_column(pid, "command=", true)
}

pub fn pid_exists(pid: u32) -> bool {
    pid > 1 && !ps_column(pid, "pid=", false).is_empty()
}

/// A plain HTTP GET to a loopback URL with a total time limit. `None` when
/// nothing answered. Like `curl -fsS`, a status of 400 or more counts as
/// failure in [`http_ok`].
pub fn http_get(url: &str, limit: Duration) -> Option<(u16, Vec<u8>)> {
    http_request("GET", url, limit, Auth::None, None)
}

/// An empty-bodied `POST`, with a bearer token unless `bearer` is empty.
pub fn http_post_auth(url: &str, limit: Duration, bearer: &str) -> Option<(u16, Vec<u8>)> {
    let auth = if bearer.is_empty() {
        Auth::None
    } else {
        Auth::Bearer(bearer)
    };
    http_request("POST", url, limit, auth, None)
}

pub fn http_get_auth(url: &str, limit: Duration, bearer: &str) -> Option<(u16, Vec<u8>)> {
    http_request("GET", url, limit, Auth::Bearer(bearer), None)
}

/// A GET to the device runner, signed with its per-launch token (see
/// [`crate::runner_token`]). Unsigned when there is no token, which only an
/// older runner (no request signing) still answers.
pub fn runner_get(
    url: &str,
    limit: Duration,
    auth: &crate::runner_token::TokenSource,
) -> Option<(u16, Vec<u8>)> {
    http_request("GET", url, limit, Auth::Runner(auth), None)
}

/// [`runner_get`] answered below 400.
pub fn runner_ok(url: &str, limit: Duration, auth: &crate::runner_token::TokenSource) -> bool {
    runner_get(url, limit, auth).is_some_and(|(status, _)| status < 400)
}

/// Read at most `max_body` bytes of the runner's body (an MJPEG stream never
/// ends).
pub fn runner_get_prefix(
    url: &str,
    limit: Duration,
    max_body: usize,
    auth: &crate::runner_token::TokenSource,
) -> Option<(u16, Vec<u8>)> {
    http_request("GET", url, limit, Auth::Runner(auth), Some(max_body))
}

#[derive(Clone, Copy)]
enum Auth<'a> {
    None,
    Bearer(&'a str),
    Runner(&'a crate::runner_token::TokenSource),
}

fn http_request(
    method: &str,
    url: &str,
    limit: Duration,
    auth: Auth<'_>,
    max_body: Option<usize>,
) -> Option<(u16, Vec<u8>)> {
    let rest = url.strip_prefix("http://")?;
    let (authority, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    let deadline = Instant::now() + limit;
    let address = std::net::ToSocketAddrs::to_socket_addrs(authority)
        .ok()?
        .next()?;
    let mut stream = std::net::TcpStream::connect_timeout(&address, limit).ok()?;
    let remaining = |deadline: Instant| {
        deadline
            .saturating_duration_since(Instant::now())
            .max(Duration::from_millis(1))
    };
    stream.set_write_timeout(Some(remaining(deadline))).ok()?;
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {authority}\r\nAccept: */*\r\nConnection: close\r\nUser-Agent: iphone-use-setup\r\n"
    );
    if method != "GET" {
        // The daemon refuses a state-changing POST without this header.
        request.push_str("Content-Length: 0\r\nX-Phone-Control: 1\r\n");
    }
    match auth {
        Auth::None => {}
        Auth::Bearer(token) => request.push_str(&format!("Authorization: Bearer {token}\r\n")),
        Auth::Runner(source) => {
            request.push_str(&crate::runner_token::header_line(source, method, path, b""))
        }
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).ok()?;
    let mut received = Vec::new();
    let mut buffer = [0u8; 16 * 1024];
    let mut header_end = None;
    // Like `curl -m`, a transfer still running at the deadline failed — an
    // endless MJPEG stream is not an answer to `/status`. Only a caller that
    // asked for a body prefix stops early on purpose.
    let mut complete = false;
    loop {
        if Instant::now() >= deadline {
            break;
        }
        stream.set_read_timeout(Some(remaining(deadline))).ok()?;
        match stream.read(&mut buffer) {
            Ok(0) => {
                complete = true;
                break;
            }
            Ok(read) => received.extend_from_slice(&buffer[..read]),
            Err(_) => break,
        }
        if header_end.is_none() {
            header_end = find_subsequence(&received, b"\r\n\r\n").map(|i| i + 4);
        }
        if let (Some(end), Some(max)) = (header_end, max_body) {
            if received.len() >= end + max {
                complete = true;
                break;
            }
        }
    }
    if !complete {
        return None;
    }
    let end = header_end.or_else(|| find_subsequence(&received, b"\r\n\r\n").map(|i| i + 4))?;
    let head = String::from_utf8_lossy(&received[..end]);
    let status = head
        .lines()
        .next()?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    let mut body = received[end..].to_vec();
    if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
        && max_body.is_none()
    {
        body = dechunk(&body);
    }
    Some((status, body))
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn dechunk(mut body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(line_end) = find_subsequence(body, b"\r\n") {
        let size_text = String::from_utf8_lossy(&body[..line_end]);
        let Ok(size) = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16)
        else {
            break;
        };
        body = &body[line_end + 2..];
        if size == 0 || body.len() < size {
            out.extend_from_slice(&body[..size.min(body.len())]);
            break;
        }
        out.extend_from_slice(&body[..size]);
        body = body.get(size + 2..).unwrap_or(&[]);
    }
    out
}

/// `curl -fsS -m <limit> <url>` succeeded: something answered below 400.
pub fn http_ok(url: &str, limit: Duration) -> bool {
    http_get(url, limit).is_some_and(|(status, _)| status < 400)
}

/// Anything accepts a TCP connection on loopback `port` right now.
pub fn tcp_listening(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_millis(200),
    )
    .is_ok()
}

/// A property list as a [`Value`]; any format `plutil` reads (XML, binary,
/// comments included) is converted first.
pub fn read_plist(path: &Path) -> Option<Value> {
    if !path.is_file() {
        return None;
    }
    let out = Command::new("/usr/bin/plutil")
        .args(["-convert", "xml1", "-o", "-"])
        .arg(path)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    crate::usbmux::parse_plist(&String::from_utf8_lossy(&out.stdout)).ok()
}

/// A scalar as `PlistBuddy -c Print` would show it: strings as-is, numbers in
/// decimal, booleans as true/false; nothing for containers or a missing key.
pub fn plist_scalar(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Int(number)) => number.to_string(),
        Some(Value::Bool(flag)) => flag.to_string(),
        _ => String::new(),
    }
}

/// `EnvironmentVariables:<key>` of a LaunchAgent plist; empty when absent.
pub fn plist_env(path: &Path, key: &str) -> String {
    let read = |key: &str| {
        read_plist(path)
            .map(|plist| {
                plist_scalar(
                    plist
                        .get("EnvironmentVariables")
                        .and_then(|env| env.get(key)),
                )
            })
            .unwrap_or_default()
    };
    let canonical = key
        .strip_prefix("PHONE_REMOTE_")
        .map(|suffix| format!("IPHONE_USE_{suffix}"))
        .unwrap_or_else(|| key.to_owned());
    let value = read(&canonical);
    if !value.is_empty() {
        return value;
    }
    core::env::legacy_names(&canonical)
        .into_iter()
        .map(|old| read(&old))
        .find(|old| !old.is_empty())
        .unwrap_or_default()
}

/// A top-level scalar (`Label`) or `ProgramArguments` item of a plist.
pub fn plist_top(path: &Path, key: &str) -> String {
    read_plist(path)
        .map(|plist| plist_scalar(plist.get(key)))
        .unwrap_or_default()
}

pub fn plist_program_argument(path: &Path, index: usize) -> String {
    read_plist(path)
        .and_then(|plist| {
            plist
                .get("ProgramArguments")
                .and_then(Value::as_array)
                .and_then(|items| items.get(index))
                .map(|item| plist_scalar(Some(item)))
        })
        .unwrap_or_default()
}

/// `defaults read <domain> <key>`, trimmed; empty when unset.
pub fn defaults_read(domain: &str, key: &str) -> String {
    Command::new("defaults")
        .args(["read", domain, key])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .unwrap_or_default()
}

/// Run a future to completion on a private current-thread runtime.
pub fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread tokio runtime")
        .block_on(future)
}

/// stdout is a terminal (an interactive run, not launchd or a pipe).
pub fn stdout_is_tty() -> bool {
    // SAFETY: isatty only reads the descriptor's state.
    unsafe { libc::isatty(libc::STDOUT_FILENO) == 1 }
}

/// Owner uid and permission bits (`%Lp`) of a path, not following a link.
pub fn owner_and_mode(path: &Path) -> Option<(u32, u32)> {
    use std::os::unix::fs::MetadataExt as _;
    let meta = std::fs::symlink_metadata(path).ok()?;
    Some((meta.uid(), meta.mode() & 0o7777))
}

/// A regular file (not a symlink) owned by this user with mode 0600.
pub fn marker_file_secure(path: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    meta.file_type().is_file() && owner_and_mode(path) == Some((uid(), 0o600))
}

/// Write `contents` to `path` atomically (temp file in the same directory,
/// `mode`, fsync, rename). Refuses to replace a symlink.
pub fn write_atomic(path: &Path, contents: &[u8], mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    if std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err(std::io::Error::other(format!(
            "refusing to replace symlinked {}",
            path.display()
        )));
    }
    let directory = path.parent().unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut temporary = tempfile::Builder::new()
        .prefix(&format!(".{name}."))
        .tempfile_in(directory)?;
    temporary.write_all(contents)?;
    temporary.as_file().sync_all()?;
    std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(mode))?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_reads_branded_ports_and_credentials_from_plists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fixture.plist");
        std::fs::write(&path,r#"<?xml version="1.0"?><plist version="1.0"><dict><key>EnvironmentVariables</key><dict><key>IPHONE_USE_PORT</key><string>45432</string><key>PHONE_REMOTE_PORT</key><string>44321</string><key>IPHONE_USE_AGENT_TOKEN</key><string>synthetic-token</string><key>IPU_RUNNER_PORT</key><string>8100</string></dict></dict></plist>"#).unwrap();
        assert_eq!(plist_env(&path, "PHONE_REMOTE_PORT"), "45432");
        assert_eq!(
            plist_env(&path, "PHONE_REMOTE_AGENT_TOKEN"),
            "synthetic-token"
        );
        assert_eq!(plist_env(&path, "IPU_RUNNER_PORT"), "8100");
    }
    /// devicectl can hang forever on a wedged tunnel; every call is bounded.
    #[test]
    fn a_hung_child_is_killed_at_its_deadline() {
        let started = Instant::now();
        let (_, ok) = run_bounded("/bin/sleep", &["30"], Duration::from_millis(500));
        assert!(!ok);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        let (out, ok) = run_bounded("/bin/echo", &["hi"], Duration::from_secs(5));
        assert!(ok);
        assert_eq!(out.trim(), "hi");
    }

    #[test]
    fn timeout_kills_wrapper_descendants_that_hold_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("escaped-timeout");
        let descendant_started = dir.path().join("descendant-started");
        let started = Instant::now();
        let (out, ok) = run_bounded(
            "/bin/sh",
            &[
                "-c",
                "(printf started > \"$2\"; sleep 2; printf escaped > \"$1\") & printf started; wait",
                "fixture",
                marker.to_str().unwrap(),
                descendant_started.to_str().unwrap(),
            ],
            Duration::from_millis(200),
        );
        assert!(!ok);
        assert_eq!(out, "started");
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(descendant_started.exists(), "the descendant did not start");
        // The descendant must be stopped too, not just detached from a
        // leaked reader thread. Its delayed write would otherwise escape.
        std::thread::sleep(Duration::from_millis(2200));
        assert!(!marker.exists(), "a timed-out descendant kept running");
    }

    #[test]
    fn successful_wrapper_does_not_wait_for_descendant_pipe_eof() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("background-finished");
        let started = Instant::now();
        let (out, ok) = run_bounded(
            "/bin/sh",
            &[
                "-c",
                "(sleep 2; printf finished > \"$1\") & printf ready",
                "fixture",
                marker.to_str().unwrap(),
            ],
            Duration::from_millis(300),
        );
        assert!(ok);
        assert_eq!(out, "ready");
        assert!(started.elapsed() < Duration::from_secs(2));
        // A successful command's background helper is not ours to kill.
        // Let this fixture finish before its temporary directory is removed.
        std::thread::sleep(Duration::from_millis(2200));
        assert_eq!(std::fs::read_to_string(marker).unwrap(), "finished");
    }

    #[test]
    fn cleanup_keeps_the_exited_group_leader_waitable_until_descendants_stop() {
        use std::os::unix::process::CommandExt as _;
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("escaped-cleanup");
        let ready = dir.path().join("descendant-started");
        let mut child = Command::new("/bin/sh")
            .args([
                "-c",
                "(printf ready > \"$2\"; sleep 2; printf escaped > \"$1\") & while [ ! -f \"$2\" ]; do sleep 0.01; done",
                "fixture",
                marker.to_str().unwrap(),
                ready.to_str().unwrap(),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while !command_exit_pending(&child).unwrap() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(ready.exists(), "the descendant must hold stdout open");
        assert!(command_exit_pending(&child).unwrap());
        assert!(
            command_exit_pending(&child).unwrap(),
            "checking again must not have reaped the leader"
        );
        kill_command_group(&mut child);
        assert!(child.wait().unwrap().success(), "the leader had exited normally");
        std::thread::sleep(Duration::from_millis(2200));
        assert!(!marker.exists(), "cleanup must stop the descendant");
    }

    #[test]
    fn bounded_command_drains_output_larger_than_the_pipe() {
        let (out, ok) = run_bounded(
            "/bin/dd",
            &["if=/dev/zero", "bs=1024", "count=256"],
            Duration::from_secs(5),
        );
        assert!(ok);
        assert_eq!(out.len(), 256 * 1024);
        assert!(out.bytes().all(|byte| byte == 0));
    }

    /// The runner handoff is a POST the daemon accepts: bearer, the control
    /// header, and an empty body it does not wait for.
    #[test]
    fn a_handoff_post_carries_what_the_daemon_requires() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://{}/agent/runner-handoff",
            listener.local_addr().unwrap()
        );
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 1024];
            while find_subsequence(&request, b"\r\n\r\n").is_none() {
                let read = stream.read(&mut buffer).unwrap();
                assert!(read > 0, "the request ended before its headers");
                request.extend_from_slice(&buffer[..read]);
            }
            let body = r#"{"up":true}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            String::from_utf8(request).unwrap()
        });
        let (status, body) = http_post_auth(&url, Duration::from_secs(5), "tok").unwrap();
        assert_eq!(status, 200);
        assert_eq!(body, br#"{"up":true}"#);
        let request = server.join().unwrap();
        assert!(
            request.starts_with("POST /agent/runner-handoff HTTP/1.1\r\n"),
            "{request}"
        );
        assert!(
            request.contains("Authorization: Bearer tok\r\n"),
            "{request}"
        );
        assert!(request.contains("X-Phone-Control: 1\r\n"), "{request}");
        assert!(request.contains("Content-Length: 0\r\n"), "{request}");
    }
}
