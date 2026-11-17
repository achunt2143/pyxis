//! Process Management Service
//! 
//! Provides process spawning, killing, and information retrieval.

use serde_json::{json, Value};
use anyhow::{Result, anyhow};
use std::process::{Command, Stdio};
use std::fs;
use std::path::Path;

pub struct ProcessService;

impl ProcessService {
    pub fn spawn(command: &str, args: Option<Vec<String>>, _options: Option<Value>) -> Result<Value> {
        let args = args.unwrap_or_default();
        
        let child = Command::new(command)
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| anyhow!("Failed to spawn process: {}", e))?;

        let pid = child.id();
        // Nothing else waits on this child, so a thread does, or it would
        // stay a zombie for echo-daemon's lifetime. (AppHost's reaper waits
        // only on the apps it launched.)
        let mut child = child;
        std::thread::spawn(move || {
            let _ = child.wait();
        });

        Ok(json!({
            "success": true,
            "pid": pid,
            "command": command,
            "args": args
        }))
    }

    pub fn kill(pid: u32, signal: Option<&str>) -> Result<Value> {
        let arg = kill_signal_arg(signal.unwrap_or("TERM"))?;

        let mut cmd = Command::new("kill");
        cmd.arg(format!("-{}", arg))
            .arg(pid.to_string());

        cmd.output()
            .map_err(|e| anyhow!("Failed to kill process {}: {}", pid, e))?;

        Ok(json!({
            "success": true,
            "pid": pid,
            "signal": signal_display(&arg)
        }))
    }

    pub fn kill_group(pgid: u32, signal: Option<&str>) -> Result<Value> {
        let arg = kill_signal_arg(signal.unwrap_or("TERM"))?;

        Command::new("kill")
            .arg(format!("-{}", arg))
            .arg(format!("-{}", pgid))
            .output()
            .map_err(|e| anyhow!("Failed to kill process group {}: {}", pgid, e))?;

        Ok(json!({
            "success": true,
            "pgid": pgid,
            "signal": signal_display(&arg)
        }))
    }

    pub fn list() -> Result<Value> {
        let output = Command::new("ps")
            .args(&["-aux", "--no-headers"])
            .output()
            .map_err(|e| anyhow!("Failed to list processes: {}", e))?;

        let output_str = String::from_utf8_lossy(&output.stdout);
        let mut processes = Vec::new();

        for line in output_str.lines() {
            if let Some(proc_info) = parse_ps_line(line) {
                processes.push(proc_info);
            }
        }

        Ok(json!({
            "processes": processes,
            "count": processes.len()
        }))
    }

    pub fn get_info(pid: u32) -> Result<Value> {
        let status_path = format!("/proc/{}/status", pid);
        
        if !Path::new(&status_path).exists() {
            return Err(anyhow!("Process {} not found", pid));
        }

        let status = fs::read_to_string(&status_path)?;
        let mut info = json!({
            "pid": pid,
            "ppid": 0,
            "name": "",
            "executable": "",
            "state": "",
            "user": "",
            "uid": 0,
            "gid": 0,
            "threads": 0,
            "memory": 0,
            "cpuUsage": 0.0,
            "startTime": 0,
            "uptime": 0
        });

        for line in status.lines() {
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() >= 2 {
                let key = parts[0].trim_end_matches(':');
                let value = parts[1].trim();

                match key {
                    "Name" => info["name"] = Value::String(value.to_string()),
                    "State" => info["state"] = Value::String(value.to_string()),
                    "PPid" => info["ppid"] = Value::Number(value.parse().unwrap_or(0).into()),
                    "Uid" => {
                        if let Some(uid) = value.split_whitespace().next() {
                            info["uid"] = Value::Number(uid.parse().unwrap_or(0).into());
                        }
                    },
                    "Gid" => {
                        if let Some(gid) = value.split_whitespace().next() {
                            info["gid"] = Value::Number(gid.parse().unwrap_or(0).into());
                        }
                    },
                    "Threads" => info["threads"] = Value::Number(value.parse().unwrap_or(0).into()),
                    "VmRSS" => {
                        if let Some(mem) = value.split_whitespace().next() {
                            let bytes: u64 = mem.parse().unwrap_or(0) * 1024;
                            info["memory"] = Value::Number(bytes.into());
                        }
                    },
                    _ => {}
                }
            }
        }

        Ok(info)
    }
}

/// Signal names `kill` may be asked for. Anything else is refused.
const KNOWN_SIGNALS: &[&str] = &["TERM", "KILL", "INT", "HUP", "STOP", "CONT", "USR1", "USR2"];

/// Turn caller text into the argument `kill` takes after its dash: a known
/// signal name (any case, with or without `SIG`) or a number 1-64. Anything
/// else is refused, so caller text never reaches `kill`'s argv unchecked.
fn kill_signal_arg(signal: &str) -> Result<String> {
    let s = signal.trim();
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) {
        return match s.parse::<u8>() {
            Ok(n) if (1..=64).contains(&n) => Ok(n.to_string()),
            _ => Err(anyhow!("Signal number must be 1-64, got `{}`", s)),
        };
    }
    let upper = s.to_ascii_uppercase();
    let name = upper.strip_prefix("SIG").unwrap_or(&upper);
    if KNOWN_SIGNALS.contains(&name) {
        Ok(name.to_string())
    } else {
        Err(anyhow!("Unsupported signal `{}`", s))
    }
}

/// Reply form of a validated signal: `SIGTERM` for a name, `15` for a number.
fn signal_display(arg: &str) -> String {
    if arg.bytes().all(|b| b.is_ascii_digit()) {
        arg.to_string()
    } else {
        format!("SIG{}", arg)
    }
}

fn parse_ps_line(line: &str) -> Option<Value> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    
    if parts.len() < 11 {
        return None;
    }

    Some(json!({
        "user": parts[0],
        "pid": parts[1].parse::<u32>().ok(),
        "cpu": parts[2],
        "memory": parts[3],
        "vsz": parts[4].parse::<u64>().ok(),
        "rss": parts[5].parse::<u64>().ok(),
        "tty": parts[6],
        "stat": parts[7],
        "start": parts[8],
        "time": parts[9],
        "command": parts[10..].join(" ")
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_known_signal_names_in_any_form() {
        assert_eq!(kill_signal_arg("TERM").unwrap(), "TERM");
        assert_eq!(kill_signal_arg("SIGKILL").unwrap(), "KILL");
        assert_eq!(kill_signal_arg("sigint").unwrap(), "INT");
        assert_eq!(kill_signal_arg("Hup").unwrap(), "HUP");
        assert_eq!(kill_signal_arg("usr1").unwrap(), "USR1");
        assert_eq!(kill_signal_arg("SIGCONT").unwrap(), "CONT");
        assert_eq!(kill_signal_arg("STOP").unwrap(), "STOP");
    }

    #[test]
    fn accepts_numbers_from_1_to_64() {
        assert_eq!(kill_signal_arg("1").unwrap(), "1");
        assert_eq!(kill_signal_arg("15").unwrap(), "15");
        assert_eq!(kill_signal_arg("64").unwrap(), "64");
    }

    #[test]
    fn refuses_everything_else() {
        for bad in [
            "", "SIG", "0", "65", "1000", "-9", "TERM -9", "KILL;reboot",
            "$(id)", "--", "-TERM", "SIGFOO", "9 HUP", "`reboot`",
        ] {
            assert!(kill_signal_arg(bad).is_err(), "should refuse {:?}", bad);
        }
    }

    #[test]
    fn kill_refuses_a_bad_signal_before_running_anything() {
        assert!(ProcessService::kill(1, Some("-9 --all")).is_err());
        assert!(ProcessService::kill_group(1, Some("bogus")).is_err());
    }
}
