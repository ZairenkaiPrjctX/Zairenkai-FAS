// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 ZairenkaiPrjctX contributors
//! Local control socket.
//!
//! A Unix stream socket, mode 0600, accepting one request line and answering one JSON line.
//! Only peers running as root or as the daemon's own uid are served (checked with
//! `SO_PEERCRED`). Requests are a fixed command set; nothing is ever passed to a shell.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use zfas_core::policy::ProfileName;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Status,
    Enable,
    Disable,
    Profile(ProfileName),
    Reload,
    /// Drop boosts and history now (keeps running).
    Reset,
}

pub fn parse_request(line: &str) -> Result<Request, String> {
    let mut it = line.split_whitespace();
    let cmd = it.next().ok_or("empty request")?;
    let arg = it.next();
    if it.next().is_some() {
        return Err("too many arguments".into());
    }
    Ok(match (cmd, arg) {
        ("status", None) => Request::Status,
        ("enable", None) => Request::Enable,
        ("disable", None) => Request::Disable,
        ("reload", None) => Request::Reload,
        ("reset", None) => Request::Reset,
        ("profile", Some(p)) => {
            Request::Profile(ProfileName::parse(p).ok_or_else(|| format!("unknown profile {p:?}"))?)
        }
        _ => return Err(format!("unknown command {cmd:?}")),
    })
}

fn peer_uid(s: &UnixStream) -> Option<u32> {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: valid fd and correctly sized out-parameter.
    let r = unsafe {
        libc::getsockopt(
            s.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut libc::ucred as *mut libc::c_void,
            &mut len,
        )
    };
    (r == 0).then_some(cred.uid)
}

pub struct Server {
    listener: UnixListener,
    path: PathBuf,
    own_uid: u32,
}

impl Server {
    pub fn bind(path: &Path) -> std::io::Result<Self> {
        if let Ok(m) = std::fs::symlink_metadata(path) {
            if m.file_type().is_socket() {
                std::fs::remove_file(path)?;
            } else {
                return Err(std::io::Error::other(format!(
                    "{} exists and is not a socket",
                    path.display()
                )));
            }
        }
        let listener = UnixListener::bind(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        // SAFETY: getuid has no preconditions.
        let own_uid = unsafe { libc::getuid() };
        Ok(Self {
            listener,
            path: path.to_path_buf(),
            own_uid,
        })
    }

    /// Accept pending connections without blocking; `handle` answers each request.
    pub fn poll(&self, mut handle: impl FnMut(Result<Request, String>) -> String) {
        for _ in 0..8 {
            let Ok((mut s, _)) = self.listener.accept() else {
                return;
            };
            let _ = s.set_nonblocking(false);
            let _ = s.set_read_timeout(Some(Duration::from_millis(200)));
            let _ = s.set_write_timeout(Some(Duration::from_millis(200)));
            let reply = match peer_uid(&s) {
                Some(uid) if uid == 0 || uid == self.own_uid => {
                    let mut line = String::new();
                    let _ = BufReader::new((&s).take(256)).read_line(&mut line);
                    handle(parse_request(line.trim()))
                }
                _ => r#"{"ok":false,"error":"permission denied"}"#.to_string(),
            };
            let _ = s.write_all(reply.as_bytes());
            let _ = s.write_all(b"\n");
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub fn request(path: &Path, line: &str) -> std::io::Result<String> {
    let mut s = UnixStream::connect(path)?;
    s.set_read_timeout(Some(Duration::from_secs(3)))?;
    s.write_all(line.as_bytes())?;
    s.write_all(b"\n")?;
    let mut out = String::new();
    BufReader::new(s.take(1 << 20)).read_line(&mut out)?;
    Ok(out.trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_parsing_is_strict() {
        assert_eq!(parse_request("status"), Ok(Request::Status));
        assert_eq!(
            parse_request("profile performance"),
            Ok(Request::Profile(ProfileName::Performance))
        );
        assert!(parse_request("profile turbo").is_err());
        assert!(parse_request("status now").is_err());
        assert!(parse_request("sh -c reboot").is_err());
        assert!(parse_request("").is_err());
    }

    #[test]
    fn roundtrip_over_socket() {
        let dir = zfas_platform::testutil::TempDir::new("sock");
        let p = dir.path().join("zfasd.sock");
        let server = Server::bind(&p).unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let client = std::thread::spawn({
            let p = p.clone();
            move || request(&p, "profile balanced").unwrap()
        });
        let mut got = None;
        for _ in 0..200 {
            server.poll(|r| {
                got = Some(r.clone());
                "{\"ok\":true}".into()
            });
            if got.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(client.join().unwrap(), "{\"ok\":true}");
        assert_eq!(got, Some(Ok(Request::Profile(ProfileName::Balanced))));
        drop(server);
        assert!(!p.exists());
    }

    #[test]
    fn refuses_to_replace_regular_file() {
        let dir = zfas_platform::testutil::TempDir::new("sock2");
        let p = dir.path().join("zfasd.sock");
        std::fs::write(&p, "not a socket").unwrap();
        assert!(Server::bind(&p).is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "not a socket");
    }
}
