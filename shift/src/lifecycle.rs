use anyhow::Result;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use chrono::Utc;
use tracing;

/// Lifecycle event tracking for processes
pub struct ProcessLifecycle {
    events: Arc<DashMap<u32, Vec<LifecycleEventRecord>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LifecycleEvent {
    Created,
    Started,
    Running,
    Suspended,
    Resumed,
    Terminated,
    Exited,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LifecycleEventRecord {
    pub event: LifecycleEvent,
    pub timestamp: String,
    pub details: Option<String>,
}

impl ProcessLifecycle {
    pub fn new() -> Self {
        ProcessLifecycle {
            events: Arc::new(DashMap::new()),
        }
    }

    pub fn record_event(&self, pid: u32, event: LifecycleEvent) -> Result<()> {
        let record = LifecycleEventRecord {
            event,
            timestamp: Utc::now().to_rfc3339(),
            details: None,
        };

        self.events
            .entry(pid)
            .or_insert_with(Vec::new)
            .push(record);

        tracing::debug!("Lifecycle event recorded for process {}: {:?}", pid, event);
        Ok(())
    }

    pub fn record_event_with_details(
        &self,
        pid: u32,
        event: LifecycleEvent,
        details: &str,
    ) -> Result<()> {
        let record = LifecycleEventRecord {
            event,
            timestamp: Utc::now().to_rfc3339(),
            details: Some(details.to_string()),
        };

        self.events
            .entry(pid)
            .or_insert_with(Vec::new)
            .push(record);

        tracing::debug!(
            "Lifecycle event recorded for process {}: {:?} - {}",
            pid,
            event,
            details
        );
        Ok(())
    }

    pub fn get_events(&self, pid: u32) -> Vec<LifecycleEventRecord> {
        self.events
            .get(&pid)
            .map(|entry| entry.clone())
            .unwrap_or_default()
    }

    pub fn get_latest_event(&self, pid: u32) -> Option<LifecycleEventRecord> {
        self.events.get(&pid).and_then(|entry| entry.last().cloned())
    }

    pub fn clear_events(&self, pid: u32) {
        self.events.remove(&pid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lifecycle_recording() {
        let lifecycle = ProcessLifecycle::new();
        lifecycle.record_event(1001, LifecycleEvent::Started).unwrap();

        let events = lifecycle.get_events(1001);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, LifecycleEvent::Started);
    }

    #[test]
    fn test_multiple_events() {
        let lifecycle = ProcessLifecycle::new();
        lifecycle.record_event(1001, LifecycleEvent::Started).unwrap();
        lifecycle.record_event(1001, LifecycleEvent::Running).unwrap();
        lifecycle.record_event(1001, LifecycleEvent::Suspended).unwrap();

        let events = lifecycle.get_events(1001);
        assert_eq!(events.len(), 3);
    }

    #[test]
    fn test_latest_event() {
        let lifecycle = ProcessLifecycle::new();
        lifecycle.record_event(1001, LifecycleEvent::Started).unwrap();
        lifecycle.record_event(1001, LifecycleEvent::Running).unwrap();

        let latest = lifecycle.get_latest_event(1001).unwrap();
        assert_eq!(latest.event, LifecycleEvent::Running);
    }
}
