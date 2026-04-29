/// Telemetry — pi opts to send anonymous metrics. We follow the on/off
/// switch (`PI_TELEMETRY=0`) but our default is always to be a no-op,
/// matching the spirit of running locally with no surprises.
pub fn enabled() -> bool {
    match std::env::var("PI_TELEMETRY").ok().as_deref() {
        Some("0") | Some("false") | Some("no") => false,
        _ => true,
    }
}

pub fn record_event(_name: &str, _props: serde_json::Value) {
    if !enabled() {
    }
    // No-op: we deliberately don't ship a network endpoint.
}
