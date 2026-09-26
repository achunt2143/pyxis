//! Audio output: volume, mute, and which output plays.
//!
//! The sound server is PipeWire, run by geckoOS-boot as the session user,
//! with WirePlumber as its session manager. That stack was chosen because
//! vela plays through PulseAudio only (its build defines MOZ_PULSEAUDIO, not
//! MOZ_ALSA), and PipeWire's Pulse server is how it gets heard. This service
//! is a thin layer over WirePlumber's own CLI: `wpctl` for the default sink's
//! volume and mute and for choosing the default, and `pw-dump` (the
//! graph as JSON) to list the outputs. Every change is signalled, so the
//! quick-settings slider and Settings stay in step with each other.

use std::process::Stdio;

use anyhow::{anyhow, Result};
use serde::Serialize;
use serde_json::Value;

use super::sound::as_session_user;

pub const AUDIO_IFACE: &str = "org.geckoOS.audio";
pub const SIGNAL_VOLUME_CHANGED: &str = "VolumeChanged";
pub const SIGNAL_OUTPUT_CHANGED: &str = "OutputChanged";

const DEFAULT_SINK: &str = "@DEFAULT_AUDIO_SINK@";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Volume {
    /// 0–100. The API only sets 0–100 (`wpctl -l 1.0`). A level another
    /// client raised above 100% reads back as 100.
    pub volume: u8,
    pub muted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Output {
    /// PipeWire's object id: what `setDefaultOutput` takes.
    pub id: u32,
    pub name: String,
    pub description: String,
    pub is_default: bool,
}

pub struct AudioService;

impl AudioService {
    pub async fn volume() -> Result<Volume> {
        parse_volume(&run("wpctl", &["get-volume", DEFAULT_SINK]).await?)
    }

    pub async fn set_volume(volume: u8) -> Result<Volume> {
        let level = format!("{:.2}", f64::from(volume.min(100)) / 100.0);
        run("wpctl", &["set-volume", "-l", "1.0", DEFAULT_SINK, &level]).await?;
        Self::volume().await
    }

    pub async fn set_mute(muted: bool) -> Result<Volume> {
        run("wpctl", &["set-mute", DEFAULT_SINK, if muted { "1" } else { "0" }]).await?;
        Self::volume().await
    }

    pub async fn outputs() -> Result<Vec<Output>> {
        let dump: Value = serde_json::from_str(&run("pw-dump", &[]).await?)
            .map_err(|e| anyhow!("pw-dump gave no JSON: {e}"))?;
        Ok(parse_outputs(&dump))
    }

    /// Make `id` the default output. It must be one of `outputs()`: `wpctl
    /// set-default` takes any object id, and would happily make a microphone
    /// "the default output".
    pub async fn set_default_output(id: u32) -> Result<Vec<Output>> {
        if !Self::outputs().await?.iter().any(|o| o.id == id) {
            return Err(anyhow!("no audio output with id {id}"));
        }
        run("wpctl", &["set-default", &id.to_string()]).await?;
        Self::outputs().await
    }
}

/// Run a PipeWire CLI as the session user, and return its stdout.
async fn run(program: &str, args: &[&str]) -> Result<String> {
    let output = as_session_user(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| anyhow!("{program}: {e}"))?;
    if !output.status.success() {
        return Err(anyhow!(
            "{program} {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// `wpctl get-volume`'s one line: `Volume: 0.40`, with ` [MUTED]` appended
/// when muted.
fn parse_volume(line: &str) -> Result<Volume> {
    let level: f64 = line
        .trim()
        .strip_prefix("Volume:")
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|number| number.parse().ok())
        .ok_or_else(|| anyhow!("unexpected wpctl output: {line:?}"))?;
    Ok(Volume {
        volume: (level * 100.0).round().clamp(0.0, 100.0) as u8,
        muted: line.contains("[MUTED]"),
    })
}

/// The audio sinks in a `pw-dump`, marked with which one the "default"
/// metadata names as `default.audio.sink`.
fn parse_outputs(dump: &Value) -> Vec<Output> {
    let objects = dump.as_array().map(Vec::as_slice).unwrap_or(&[]);
    let default_name = objects
        .iter()
        .filter(|o| o["type"] == "PipeWire:Interface:Metadata" && o["props"]["metadata.name"] == "default")
        .flat_map(|o| o["metadata"].as_array().into_iter().flatten())
        .find(|entry| entry["key"] == "default.audio.sink")
        .and_then(|entry| entry["value"]["name"].as_str());
    let mut outputs: Vec<Output> = objects
        .iter()
        .filter(|o| o["type"] == "PipeWire:Interface:Node")
        .filter(|o| o["info"]["props"]["media.class"] == "Audio/Sink")
        .filter_map(|o| {
            let props = &o["info"]["props"];
            let name = props["node.name"].as_str()?.to_owned();
            Some(Output {
                id: o["id"].as_u64()? as u32,
                description: props["node.description"].as_str().unwrap_or(&name).to_owned(),
                is_default: default_name == Some(name.as_str()),
                name,
            })
        })
        .collect();
    outputs.sort_by_key(|o| o.id);
    outputs
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_wpctl_volume_lines() {
        assert_eq!(parse_volume("Volume: 0.40\n").unwrap(), Volume { volume: 40, muted: false });
        assert_eq!(parse_volume("Volume: 1.00 [MUTED]").unwrap(), Volume { volume: 100, muted: true });
        // Another client can push a sink past 100%. Report it as full.
        assert_eq!(parse_volume("Volume: 1.53").unwrap().volume, 100);
        assert!(parse_volume("Could not connect to PipeWire").is_err());
    }

    #[test]
    fn lists_sinks_only_and_marks_the_default() {
        let dump = json!([
            { "id": 32, "type": "PipeWire:Interface:Node", "info": { "props": {
                "media.class": "Audio/Sink", "node.name": "alsa_output.pci-0000_00_1b.0.analog-stereo",
                "node.description": "Built-in Audio Analog Stereo" } } },
            { "id": 33, "type": "PipeWire:Interface:Node", "info": { "props": {
                "media.class": "Audio/Source", "node.name": "alsa_input.mic" } } },
            { "id": 40, "type": "PipeWire:Interface:Node", "info": { "props": {
                "media.class": "Audio/Sink", "node.name": "hdmi" } } },
            { "id": 50, "type": "PipeWire:Interface:Metadata", "props": { "metadata.name": "default" },
              "metadata": [ { "key": "default.audio.sink",
                              "value": { "name": "alsa_output.pci-0000_00_1b.0.analog-stereo" } } ] }
        ]);
        let outputs = parse_outputs(&dump);
        assert_eq!(outputs.len(), 2, "the microphone is not an output");
        assert!(outputs[0].is_default);
        assert_eq!(outputs[0].description, "Built-in Audio Analog Stereo");
        assert!(!outputs[1].is_default);
        assert_eq!(outputs[1].description, "hdmi", "falls back to the node name");
    }

    #[test]
    fn a_graph_with_no_sinks_is_an_empty_list() {
        assert!(parse_outputs(&json!([])).is_empty());
        assert!(parse_outputs(&json!({"not": "an array"})).is_empty());
    }
}
