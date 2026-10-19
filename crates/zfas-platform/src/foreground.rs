//! Foreground app detection from `dumpsys activity activities`, and pid lookup via `/proc`.

use std::time::Duration;

use zfas_core::config::valid_package;

use crate::exec::{ExecError, Runner};
use crate::sysroot::SysRoot;

/// Package of the resumed activity. Handles the `mResumedActivity:` (Android <= 9),
/// `ResumedActivity:` / `topResumedActivity=` (Android 10+) forms.
pub fn parse_resumed_package(text: &str) -> Option<String> {
    const KEYS: [&str; 3] = [
        "topResumedActivity=",
        "mResumedActivity:",
        "ResumedActivity:",
    ];
    for key in KEYS {
        for line in text.lines() {
            let Some(idx) = line.find(key) else { continue };
            // `ActivityRecord{hash u0 com.pkg/.Activity t12}`
            let rest = &line[idx + key.len()..];
            if let Some(pkg) = rest
                .split_whitespace()
                .find(|t| t.contains('/'))
                .and_then(|t| t.split('/').next())
                .map(|p| p.trim_start_matches('{'))
            {
                if valid_package(pkg) {
                    return Some(pkg.to_string());
                }
            }
        }
    }
    None
}

pub fn foreground_package(runner: &dyn Runner) -> Result<Option<String>, ExecError> {
    let out = runner.run(
        &["dumpsys", "activity", "activities"],
        Duration::from_secs(2),
    )?;
    Ok(parse_resumed_package(&out))
}

/// Main process of `package`: the process whose `cmdline` is exactly the package name.
/// (Secondary processes are `package:name` and are not the renderer of the main window.)
pub fn pid_of(root: &SysRoot, package: &str) -> Option<u32> {
    let mut found = None;
    for name in root.list_dir("/proc").ok()? {
        let Ok(pid) = name.parse::<u32>() else {
            continue;
        };
        let Ok(raw) = std::fs::read(root.path(format!("/proc/{pid}/cmdline"))) else {
            continue;
        };
        let first = raw.split(|b| *b == 0).next().unwrap_or(&[]);
        if first == package.as_bytes() {
            // Several matches (zygote race): prefer the newest (highest) pid.
            found = Some(found.map_or(pid, |f: u32| f.max(pid)));
        }
    }
    found
}

/// Thread ids of a process.
pub fn tids_of(root: &SysRoot, pid: u32) -> Vec<u32> {
    root.list_dir(format!("/proc/{pid}/task"))
        .map(|v| v.iter().filter_map(|t| t.parse().ok()).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn parses_android_versions() {
        let a9 = "  mResumedActivity: ActivityRecord{1a2b u0 com.example.game/.MainActivity t42}\n";
        let a12 = "    topResumedActivity=ActivityRecord{9f u0 com.example.game/com.unity3d.player.UnityPlayerActivity t7}\n\
                     ResumedActivity: ActivityRecord{9f u0 com.example.game/com.unity3d.player.UnityPlayerActivity t7}\n";
        assert_eq!(
            parse_resumed_package(a9).as_deref(),
            Some("com.example.game")
        );
        assert_eq!(
            parse_resumed_package(a12).as_deref(),
            Some("com.example.game")
        );
        assert_eq!(parse_resumed_package("nothing here"), None);
        assert_eq!(
            parse_resumed_package("mResumedActivity: ActivityRecord{1 u0 $(reboot)/.A t1}"),
            None
        );
    }

    #[test]
    fn pid_and_tids() {
        let d = TempDir::new("proc");
        d.file("proc/100/cmdline", "com.example.game\0");
        d.file("proc/101/cmdline", "com.example.game:remote\0");
        d.file("proc/100/task/100/stat", "");
        d.file("proc/100/task/105/stat", "");
        d.file("proc/self/cmdline", "x");
        let root = SysRoot::at(d.path());
        assert_eq!(pid_of(&root, "com.example.game"), Some(100));
        assert_eq!(pid_of(&root, "com.other"), None);
        assert_eq!(tids_of(&root, 100), vec![100, 105]);
    }
}
