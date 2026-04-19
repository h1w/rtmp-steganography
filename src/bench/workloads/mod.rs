pub mod http_echo;
pub mod dns;
pub mod ssh_probe;
pub mod https_fetch;
pub mod throughput;

use std::sync::Arc;
use crate::tunnel::metrics::{Event, EventEmitter};

#[derive(Debug, Clone, Copy)]
pub struct WorkloadResult {
    pub ok: bool,
    pub latency_ms: u64,
    pub bytes: u64,
}

pub fn emit_done(em: &Arc<EventEmitter>, name: &'static str, id: u64, r: &WorkloadResult) {
    em.emit(Event::new("bench", "request_done")
        .field("workload", name)
        .field("id", id as i64)
        .field("ok", r.ok)
        .field("latency_ms", r.latency_ms as i64)
        .field("bytes", r.bytes as i64));
}
