//! Bounded state from pw-dump's event stream. No GTK or daemon calls.
use serde_json::Value;
use std::collections::BTreeMap;

pub const MAX_PENDING: usize = 4 * 1024 * 1024;
const MAX_OBJECTS: usize = 4096;
const MAX_STATE: usize = 16 * 1024 * 1024;

#[derive(Default)]
pub struct AudioState {
    objects: BTreeMap<u64, Value>,
    pending: Vec<u8>,
    state_bytes: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Output {
    pub sink: u64,
    pub device: Option<u64>,
    pub route: Option<(u64, u64)>,
    pub name: String,
    pub selected: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub recording: bool,
    pub source: Option<(f64, bool)>,
    pub sink: Option<(f64, bool)>,
    pub outputs: Vec<Output>,
}

impl AudioState {
    /// pw-dump emits consecutive JSON arrays, split at arbitrary pipe reads.
    /// Reject malformed or oversized streams instead of growing indefinitely.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<bool, &'static str> {
        if self.pending.len().saturating_add(bytes.len()) > MAX_PENDING {
            return Err("audio event exceeds the buffer limit");
        }
        self.pending.extend_from_slice(bytes);
        let mut consumed = 0;
        let mut batches = Vec::new();
        let mut stream = serde_json::Deserializer::from_slice(&self.pending).into_iter::<Value>();
        while let Some(value) = stream.next() {
            match value {
                Ok(Value::Array(batch)) => {
                    batches.push(batch);
                    consumed = stream.byte_offset();
                }
                Ok(_) => return Err("audio event is not an array"),
                Err(e) if e.is_eof() => break,
                Err(_) => return Err("invalid audio event JSON"),
            }
        }
        self.pending.drain(..consumed);
        let changed = !batches.is_empty();
        for batch in batches {
            for mut object in batch {
                let Some(id) = object["id"].as_u64() else {
                    continue;
                };
                if object.get("info").is_some_and(Value::is_null) {
                    if let Some(old) = self.objects.remove(&id) {
                        self.state_bytes = self.state_bytes.saturating_sub(old.to_string().len());
                    }
                    continue;
                }
                // Metadata dumps contain changed entries, unlike node/device
                // info, which contains the complete current cached state.
                if object["type"]
                    .as_str()
                    .is_some_and(|s| s.ends_with(":Metadata"))
                {
                    if let Some(old) = self.objects.get(&id) {
                        let mut entries = old["metadata"].as_array().cloned().unwrap_or_default();
                        for entry in object["metadata"].as_array().into_iter().flatten() {
                            entries.retain(|e| {
                                e["subject"] != entry["subject"] || e["key"] != entry["key"]
                            });
                            if !entry["value"].is_null() {
                                entries.push(entry.clone());
                            }
                        }
                        object["metadata"] = Value::Array(entries);
                    }
                }
                if self.objects.len() < MAX_OBJECTS || self.objects.contains_key(&id) {
                    let previous = self.objects.get(&id).map_or(0, |old| old.to_string().len());
                    let total = self
                        .state_bytes
                        .saturating_sub(previous)
                        .saturating_add(object.to_string().len());
                    if total > MAX_STATE {
                        return Err("audio graph exceeds the retained-state limit");
                    }
                    self.state_bytes = total;
                    self.objects.insert(id, object);
                }
            }
        }
        Ok(changed)
    }

    fn default_name(&self, key: &str) -> Option<String> {
        self.objects
            .values()
            .filter(|o| o["props"]["metadata.name"] == "default")
            .flat_map(|o| o["metadata"].as_array().into_iter().flatten())
            .find(|e| e["subject"] == 0 && e["key"] == key)
            .and_then(|e| {
                let value = if let Some(s) = e["value"].as_str() {
                    serde_json::from_str::<Value>(s).ok()?
                } else {
                    e["value"].clone()
                };
                value["name"].as_str().map(str::to_owned)
            })
    }

    pub fn snapshot(&self) -> Snapshot {
        let default_sink = self.default_name("default.audio.sink");
        let default_source = self.default_name("default.audio.source");
        let mut snapshot = Snapshot::default();
        for (&id, object) in &self.objects {
            let props = &object["info"]["props"];
            let class = props["media.class"].as_str().unwrap_or("");
            if class == "Stream/Input/Audio"
                && object["info"]["state"] == "running"
                && !property_bool(&props["stream.monitor"])
                && !property_bool(&props["node.passive"])
                && !["pavucontrol", "gnome-volume-control"].iter().any(|name| {
                    props["application.id"].as_str() == Some(name)
                        || props["application.name"].as_str() == Some("PulseAudio Volume Control")
                })
            {
                snapshot.recording = true;
            }
            let is_default_sink =
                props["node.name"].as_str() == default_sink.as_deref() && default_sink.is_some();
            if class == "Audio/Source"
                && default_source.is_some()
                && props["node.name"].as_str() == default_source.as_deref()
            {
                snapshot.source = node_level(object);
            }
            if class != "Audio/Sink" {
                continue;
            }
            if is_default_sink {
                snapshot.sink = node_level(object);
            }
            let name = props["node.description"]
                .as_str()
                .or_else(|| props["node.nick"].as_str())
                .unwrap_or("Audio Output");
            let device_id = number(&props["device.id"]);
            let profile_device = number(&props["card.profile.device"]);
            let mut routes = Vec::new();
            if let (Some(device_id), Some(profile_device)) = (device_id, profile_device) {
                if let Some(device) = self.objects.get(&device_id) {
                    let params = &device["info"]["params"];
                    for route in params["EnumRoute"].as_array().into_iter().flatten() {
                        if route["direction"] != "Output"
                            || route["available"] == "no"
                            || !route["devices"].as_array().is_some_and(|ids| {
                                ids.iter().any(|v| number(v) == Some(profile_device))
                            })
                        {
                            continue;
                        }
                        let Some(index) = number(&route["index"]) else {
                            continue;
                        };
                        let selected = is_default_sink
                            && params["Route"].as_array().is_some_and(|r| {
                                r.iter().any(|r| {
                                    number(&r["index"]) == Some(index)
                                        && number(&r["device"]) == Some(profile_device)
                                })
                            });
                        let port = route["description"]
                            .as_str()
                            .or_else(|| route["name"].as_str())
                            .unwrap_or("Output");
                        routes.push(Output {
                            sink: id,
                            device: Some(device_id),
                            route: Some((index, profile_device)),
                            name: format!("{port} – {name}"),
                            selected,
                        });
                    }
                }
            }
            if routes.is_empty() {
                routes.push(Output {
                    sink: id,
                    device: None,
                    route: None,
                    name: name.to_owned(),
                    selected: is_default_sink,
                });
            }
            snapshot.outputs.extend(routes);
        }
        snapshot
    }
}

fn property_bool(v: &Value) -> bool {
    v.as_bool().unwrap_or(v.as_str() == Some("true"))
}
fn number(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_str()?.parse().ok())
}
fn node_level(node: &Value) -> Option<(f64, bool)> {
    let props = node["info"]["params"]["Props"].as_array()?.first()?;
    let volume = props["channelVolumes"]
        .as_array()
        .and_then(|channels| channels.first())
        .and_then(Value::as_f64)
        .or_else(|| props["volume"].as_f64())?;
    // WirePlumber's mixer API exposes the cubic (perceptual) scale.
    Some((
        volume.max(0.0).cbrt().mul_add(100.0, 0.0).clamp(0.0, 100.0),
        props["mute"].as_bool().unwrap_or(false),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn feed(state: &mut AudioState, value: Value) {
        state.feed(value.to_string().as_bytes()).unwrap();
    }
    #[test]
    fn split_events_remove_capture_and_ignore_monitor_streams() {
        let mut s = AudioState::default();
        let event = br#"[{"id":70,"info":{"state":"running","props":{"media.class":"Stream/Input/Audio"}}}]"#;
        assert!(!s.feed(&event[..15]).unwrap());
        assert!(s.feed(&event[15..]).unwrap());
        assert!(s.snapshot().recording);
        feed(&mut s, json!([{"id":70,"info":null}]));
        assert!(!s.snapshot().recording);
        feed(
            &mut s,
            json!([{"id":70,"info":{"state":"running","props":{"media.class":"Stream/Input/Audio","stream.monitor":true}}}]),
        );
        assert!(!s.snapshot().recording);
        assert!(s.feed(b"garbage").is_err());
        assert!(AudioState::default()
            .feed(&vec![b' '; MAX_PENDING + 1])
            .is_err());
    }
    #[test]
    fn ports_share_a_sink_but_switch_distinct_routes_and_metadata_is_incremental() {
        let mut s = AudioState::default();
        feed(
            &mut s,
            json!([
                {"id":20,"type":"PipeWire:Interface:Metadata","props":{"metadata.name":"default"},"metadata":[
                    {"subject":0,"key":"default.audio.sink","value":{"name":"analog"}},
                    {"subject":0,"key":"default.audio.source","value":{"name":"mic"}}]},
                {"id":48,"info":{"props":{"media.class":"Audio/Sink","node.name":"analog","node.description":"Built-in Audio","device.id":"40","card.profile.device":0},"params":{"Props":[{"channelVolumes":[0.512,0.125],"mute":false}]}}},
                {"id":49,"info":{"props":{"media.class":"Audio/Source","node.name":"mic"},"params":{"Props":[{"channelVolumes":[0.125],"mute":true}]}}},
                {"id":40,"info":{"params":{"EnumRoute":[
                    {"index":1,"direction":"Output","description":"Speakers","available":"yes","devices":[0]},
                    {"index":2,"direction":"Output","description":"Headphones","available":"unknown","devices":[0]},
                    {"index":3,"direction":"Input","description":"Microphone","devices":[0]},
                    {"index":4,"direction":"Output","available":"no","devices":[0]}],"Route":[{"index":1,"device":0}]}}}
            ]),
        );
        let a = s.snapshot();
        assert_eq!(a.sink, Some((80.0, false)));
        assert_eq!(a.source, Some((50.0, true)));
        assert_eq!(a.outputs.len(), 2);
        assert!(a.outputs[0].selected);
        assert_eq!(a.outputs[1].route, Some((2, 0)));
        feed(
            &mut s,
            json!([{"id":20,"type":"PipeWire:Interface:Metadata","props":{"metadata.name":"default"},"metadata":[{"subject":0,"key":"default.audio.sink","value":{"name":"other"}}]}]),
        );
        assert_eq!(s.snapshot().source, a.source);
        assert!(s.snapshot().sink.is_none());
    }
}
