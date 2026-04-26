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

        Ok(json!({
            "success": true,
            "pid": pid,
            "command": command,
            "args": args
        }))
    }

    pub fn kill(pid: u32, signal: Option<&str>) -> Result<Value> {
        let signal = signal.unwrap_or("SIGTERM");
        
        let mut cmd = Command::new("kill");
        cmd.arg(format!("-{}", signal))
            .arg(pid.to_string());

        cmd.output()
            .map_err(|e| anyhow!("Failed to kill process {}: {}", pid, e))?;

        Ok(json!({
            "success": true,
            "pid": pid,
            "signal": signal
        }))
    }

    pub fn kill_group(pgid: u32, signal: Option<&str>) -> Result<Value> {
        let signal = signal.unwrap_or("SIGTERM");
        
        Command::new("kill")
            .arg(format!("-{}", signal))
            .arg(format!("-{}", pgid))
            .output()
            .map_err(|e| anyhow!("Failed to kill process group {}: {}", pgid, e))?;

        Ok(json!({
            "success": true,
            "pgid": pgid,
            "signal": signal
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
