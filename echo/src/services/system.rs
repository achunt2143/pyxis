//! System Service (`org.geckoOS.system`)
//!
//! Backs the two bridge methods that sit at the top level of the
//! `window.geckoOS` API surface every pyxis:// app sees:
//! `getSystemInfo()` -> `system.getInfo`, and `listServices()` ->
//! `system.listServices` (see
//! `gecko/patches/pyxis/toolkit/actors/PyxisBridgeChild.sys.mjs` and its
//! dev-time twin `gecko/bridge/extension/gaia-bridge.js`). Both were part of
//! the advertised surface long before anything answered them: until this
//! module existed, calling either one from an app returned
//! `Service not registered: org.geckoOS.system`, while every other namespace
//! on that object worked. Verified on device 2026-09-20.
//!
//! ## `getInfo` composes, it does not re-read
//!
//! Most of a "system info" blob -- OS, kernel, hostname, memory, CPU -- is
//! already implemented by [`DeviceInfoService`] and exposed as
//! `org.geckoOS.deviceInfo`'s per-fact methods. This service deliberately
//! calls that code rather than parsing `/proc` a second time, so there is
//! exactly one implementation per fact and the two services cannot drift
//! apart. What it adds is the part nothing else reports: the machine's
//! *runtime* state -- uptime, boot time, load average, task counts -- which
//! is read here from `/proc` directly because no other service owns it.
//!
//! A sub-fact that fails (a missing `uname`/`df`, an unreadable `/proc`
//! entry) yields `null` for its field plus an entry in `unavailable`
//! explaining why, rather than failing the whole call. A caller needs to be
//! able to tell "this machine has no DMI vendor" from "this call broke".
//!
//! ## `listServices` reads the router, not just the registry
//!
//! The non-obvious part. Echo has two disjoint notions of "a service exists":
//!
//! - [`crate::registry::ServiceRegistry`] -- populated *only* by the
//!   `RegisterService` method call, i.e. by external socket clients such as
//!   Evo (`org.geckoOS.evo`) and Shift.
//! - `MessageRouter::handlers` -- populated by `register_handler`, which is
//!   how every in-process service in `echo/src/bin/daemon.rs`'s
//!   `register_all_services` (filesystem, power, nova, installer, ...) is
//!   installed.
//!
//! Nothing writes to both. So the pre-existing `org.geckoOS.Echo`
//! `ListServices` method, which reads the registry alone, reports *none* of
//! the in-process services -- on a freshly booted device with no external
//! clients connected it answers `{"services": [], "count": 0}` while a dozen
//! services are live and answering calls. This implementation merges both
//! sources, so what it returns is what the bus can actually dispatch.
//!
//! One more honest-reporting detail: `IPCBus::remove_connection` drops a
//! disconnected client from `external_services` but leaves it in the
//! registry, so a registered-then-crashed service lingers there. Each entry
//! therefore carries `connected`, resolved per-entry against the live
//! forwarding table, instead of implying every listed service is reachable.

use anyhow::{anyhow, Result};
use chrono::{TimeZone, Utc};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

use crate::services::deviceinfo::DeviceInfoService;
use crate::IPCBus;

/// Destination prefix every Pyxis service is addressed under. The bridge's
/// `callService(service, ...)` passes the short tail (`"filesystem"`), which
/// `PyxisBridgeParent.sys.mjs` expands back to the full destination -- so a
/// listing has to report both halves for its output to be actionable.
const DESTINATION_PREFIX: &str = "org.geckoOS.";

pub struct SystemService;

impl SystemService {
    /// `org.geckoOS.system.getInfo` -> a single snapshot of the machine.
    pub fn get_info() -> Result<Value> {
        let mut unavailable = Map::new();

        let host = field(
            &mut unavailable,
            "hostname",
            DeviceInfoService::get_hostname(),
        );
        // get_hostname() returns {hostname, domain}; lift both to the top
        // level so callers read `info.hostname`, not `info.hostname.hostname`.
        let hostname = host.get("hostname").cloned().unwrap_or(Value::Null);
        let domain = host.get("domain").cloned().unwrap_or(Value::Null);

        let mut info = json!({
            "hostname": hostname,
            "domain": domain,
            "os": field(&mut unavailable, "os", DeviceInfoService::get_os()),
            "kernel": field(&mut unavailable, "kernel", DeviceInfoService::get_kernel()),
            "cpu": field(&mut unavailable, "cpu", DeviceInfoService::get_cpu_info()),
            "memory": field(&mut unavailable, "memory", DeviceInfoService::get_memory_info()),
            "uptime": field(&mut unavailable, "uptime", read_uptime()),
            "load": field(&mut unavailable, "load", read_load_average()),
        });

        // Present only when something actually failed -- an always-present
        // empty object invites callers to stop checking it.
        if !unavailable.is_empty() {
            info["unavailable"] = Value::Object(unavailable);
        }

        Ok(info)
    }

    /// `org.geckoOS.system.ping` -> a round-trip with nothing behind it.
    ///
    /// Exists so that `echo-cli ping` can time the bus rather than timing
    /// whatever work the method it happened to call does. It used to call
    /// `org.geckoOS.Echo`'s `ListServices` "as a ping", which measured a
    /// registry walk; [`Self::list_services`] below would be worse, measuring
    /// a scan of every handler on the bus plus one connection lookup per
    /// external service. A caller that wants a liveness probe wants neither.
    pub fn ping() -> Value {
        json!({ "pong": true })
    }

    /// `org.geckoOS.system.listServices` -> every service the bus can route
    /// to, merged from the router's handler table and the service registry.
    pub async fn list_services(bus: &IPCBus) -> Result<Value> {
        let mut services: BTreeMap<String, ServiceEntry> = BTreeMap::new();

        // In-process services: the router is the only record that these
        // exist at all.
        for (destination, method) in bus.router().handler_entries() {
            services
                .entry(destination)
                .or_default()
                .in_process_methods
                .push(method);
        }

        // Externally registered services: the registry additionally carries
        // identity (service_id) and registration time, plus the interface the
        // client *declared* -- which is a claim about what it implements, not
        // something this bus dispatches, hence the separate field name.
        for info in bus.list_services().await? {
            let entry = services.entry(info.service_name.clone()).or_default();
            entry.service_id = Some(info.service_id);
            entry.registered_at = Some(info.registered_at);
            entry.declared_methods = info
                .interface
                .methods
                .into_iter()
                .map(|method| method.name)
                .collect();
            // A registry entry outlives its connection (see module docs), so
            // reachability has to be resolved against the live table.
            entry.connected = bus
                .get_connection_for_service(&info.service_name)
                .await
                .is_some();
        }

        let listed: Vec<Value> = services
            .into_iter()
            .map(|(destination, entry)| entry.into_json(&destination))
            .collect();

        Ok(json!({
            "count": listed.len(),
            "services": listed,
        }))
    }
}

/// One merged service, keyed by full destination (`org.geckoOS.filesystem`).
#[derive(Default)]
struct ServiceEntry {
    /// Methods the router will dispatch in-process, right now.
    in_process_methods: Vec<String>,
    /// Methods an external client claimed in its `RegisterService` interface.
    declared_methods: Vec<String>,
    service_id: Option<String>,
    registered_at: Option<String>,
    connected: bool,
}

impl ServiceEntry {
    fn into_json(mut self, destination: &str) -> Value {
        self.in_process_methods.sort();
        self.in_process_methods.dedup();
        self.declared_methods.sort();
        self.declared_methods.dedup();

        // "builtin" = handlers compiled into echo-daemon; "external" = a
        // socket client that registered over the wire. Both at once is legal
        // (a client registering a name the daemon also handles) and worth
        // seeing rather than flattening away.
        let kind = match (
            !self.in_process_methods.is_empty(),
            self.service_id.is_some(),
        ) {
            (true, true) => "builtin+external",
            (true, false) => "builtin",
            (false, true) => "external",
            (false, false) => "unknown",
        };

        json!({
            // The short name is what an app passes to
            // `window.geckoOS.callService(<name>, ...)`, so lead with it.
            "name": destination
                .strip_prefix(DESTINATION_PREFIX)
                .unwrap_or(destination),
            "destination": destination,
            "kind": kind,
            // Always true for builtins; for an external service it is a live
            // lookup, not a restatement of the registry.
            "reachable": !self.in_process_methods.is_empty() || self.connected,
            "methods": self.in_process_methods,
            "declaredMethods": self.declared_methods,
            "serviceId": self.service_id,
            "registeredAt": self.registered_at,
        })
    }
}

/// Runs one sub-fact, recording *why* it failed instead of discarding the
/// reason or aborting the whole snapshot.
fn field(unavailable: &mut Map<String, Value>, name: &str, result: Result<Value>) -> Value {
    match result {
        Ok(value) => value,
        Err(e) => {
            unavailable.insert(name.to_string(), Value::String(e.to_string()));
            Value::Null
        }
    }
}

/// `/proc/uptime` is "<seconds since boot> <summed idle seconds>", both as
/// floats. Boot time comes from `/proc/stat`'s `btime` line (a unix
/// timestamp) rather than being derived as now-minus-uptime, which drifts
/// across suspend and clock adjustment.
fn read_uptime() -> Result<Value> {
    let raw = std::fs::read_to_string("/proc/uptime")
        .map_err(|e| anyhow!("Failed to read /proc/uptime: {}", e))?;

    let mut fields = raw.split_whitespace();
    let seconds: f64 = fields
        .next()
        .ok_or_else(|| anyhow!("/proc/uptime was empty"))?
        .parse()
        .map_err(|e| anyhow!("Unparseable uptime in /proc/uptime: {}", e))?;
    // Idle time is per-CPU-summed and absent on some kernels; not worth
    // failing the whole field over.
    let idle: Option<f64> = fields.next().and_then(|value| value.parse().ok());

    Ok(json!({
        "seconds": seconds,
        "idleSeconds": idle,
        "bootTime": read_boot_time(),
    }))
}

/// RFC3339 boot timestamp, or `null` if `/proc/stat` has no usable `btime`.
fn read_boot_time() -> Option<String> {
    let stat = std::fs::read_to_string("/proc/stat").ok()?;
    let btime: i64 = stat
        .lines()
        .find_map(|line| line.strip_prefix("btime "))?
        .trim()
        .parse()
        .ok()?;
    Utc.timestamp_opt(btime, 0).single().map(|t| t.to_rfc3339())
}

/// `/proc/loadavg` is "<1min> <5min> <15min> <runnable>/<total> <last pid>".
/// The task counts are the cheapest honest process count available -- the
/// kernel already maintains them, so there is no reason to walk `/proc` the
/// way `org.geckoOS.process`'s `list` does.
fn read_load_average() -> Result<Value> {
    let raw = std::fs::read_to_string("/proc/loadavg")
        .map_err(|e| anyhow!("Failed to read /proc/loadavg: {}", e))?;

    let fields: Vec<&str> = raw.split_whitespace().collect();
    if fields.len() < 4 {
        return Err(anyhow!("Unexpected /proc/loadavg format: {:?}", raw.trim()));
    }

    let parse_avg = |raw: &str| -> Result<f64> {
        raw.parse()
            .map_err(|e| anyhow!("Unparseable load average '{}': {}", raw, e))
    };

    let (runnable, total) = fields[3].split_once('/').ok_or_else(|| {
        anyhow!(
            "Unexpected task-count field in /proc/loadavg: {}",
            fields[3]
        )
    })?;

    Ok(json!({
        "one": parse_avg(fields[0])?,
        "five": parse_avg(fields[1])?,
        "fifteen": parse_avg(fields[2])?,
        "runnableTasks": runnable.parse::<u64>().ok(),
        "totalTasks": total.parse::<u64>().ok(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_info_reports_uptime_and_hostname() {
        let info = SystemService::get_info().expect("get_info must not fail as a whole");

        // Uptime is the field nothing else on the bus provides, and the whole
        // reason `getInfo` is more than an alias for deviceInfo.
        assert!(
            info["uptime"]["seconds"].as_f64().unwrap_or(-1.0) > 0.0,
            "expected a positive uptime, got {:?} (unavailable: {:?})",
            info["uptime"],
            info.get("unavailable")
        );
        assert!(info.get("hostname").is_some());
        assert!(info["load"]["one"].is_number());
    }

    #[test]
    fn get_info_degrades_per_field_instead_of_failing() {
        // The contract that keeps one missing binary from taking out the
        // whole snapshot: every key is present either way, and anything that
        // failed is named in `unavailable` rather than silently dropped.
        let info = SystemService::get_info().unwrap();
        for key in [
            "hostname", "os", "kernel", "cpu", "memory", "uptime", "load",
        ] {
            assert!(info.get(key).is_some(), "missing key {key}");
        }
        if let Some(unavailable) = info.get("unavailable") {
            for (field, reason) in unavailable.as_object().unwrap() {
                assert!(
                    info[field].is_null(),
                    "{field} is in unavailable but not null"
                );
                assert!(!reason.as_str().unwrap().is_empty());
            }
        }
    }

    #[tokio::test]
    async fn list_services_sees_in_process_handlers() {
        use crate::{handler, Message};

        let bus = IPCBus::new(
            std::env::temp_dir().join(format!("echo-system-test-{}.sock", uuid::Uuid::new_v4())),
        )
        .await
        .unwrap();

        bus.register_handler(
            "org.geckoOS.system",
            "getInfo",
            handler(move |msg: Message| {
                Box::pin(
                    async move { Ok(Message::method_return(msg.header.message_id, json!({}))) },
                )
            }),
        )
        .await;

        let listed = SystemService::list_services(&bus).await.unwrap();
        let services = listed["services"].as_array().unwrap();

        // The regression this guards: reading the registry alone (as
        // org.geckoOS.Echo's ListServices does) would report zero here.
        let system = services
            .iter()
            .find(|s| s["destination"] == "org.geckoOS.system")
            .expect("in-process handler must be listed");

        assert_eq!(system["name"], "system");
        assert_eq!(system["kind"], "builtin");
        assert_eq!(system["reachable"], true);
        assert_eq!(system["methods"], json!(["getInfo"]));
        assert_eq!(listed["count"].as_u64().unwrap() as usize, services.len());
    }
}
