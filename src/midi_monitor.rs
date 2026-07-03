//! A live tally of incoming MIDI messages for the `--debug` on-screen monitor.
//!
//! Written by the MIDI input thread(s) as messages arrive, read by the TUI
//! render loop. Messages are aggregated by kind (note / bend / channel
//! pressure / poly aftertouch / each CC number), each row keeping the latest
//! raw bytes and a hit count — so a controller that floods one dimension (an
//! MPE Seaboard streaming pressure, say) shows up as a single row that ticks
//! up, rather than scrolling everything else off screen. Aggregating across
//! channels keeps per-note MPE channels from proliferating into 15 rows.
//!
//! Disabled (no `--debug`) it's a no-op, so it costs nothing in normal use.

use std::collections::BTreeMap;
use std::sync::Mutex;

pub struct MidiMonitor {
    enabled: bool,
    /// kind label → (latest raw bytes, hit count).
    tally: Mutex<BTreeMap<String, (String, u64)>>,
}

impl MidiMonitor {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            tally: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Record one raw MIDI message. No-op when disabled.
    pub fn record(&self, bytes: &[u8]) {
        if !self.enabled || bytes.is_empty() {
            return;
        }
        let key = match bytes[0] & 0xF0 {
            0x90 => "Note On".to_string(),
            0x80 => "Note Off".to_string(),
            0xA0 => "Poly Aftertouch".to_string(),
            0xB0 => format!("CC {}", bytes.get(1).copied().unwrap_or(0)),
            0xC0 => "Program".to_string(),
            0xD0 => "Channel Pressure".to_string(),
            0xE0 => "Pitch Bend".to_string(),
            other => format!("status {other:#04x}"),
        };
        let detail = format!("{bytes:02x?}");
        if let Ok(mut tally) = self.tally.lock() {
            let entry = tally.entry(key).or_insert_with(|| (String::new(), 0));
            entry.0 = detail;
            entry.1 = entry.1.saturating_add(1);
        }
    }

    /// Formatted rows (`kind  latest-bytes  xN`), one per message kind seen,
    /// for display. Empty until the first message arrives.
    pub fn snapshot(&self) -> Vec<String> {
        self.tally
            .lock()
            .map(|tally| {
                tally
                    .iter()
                    .map(|(kind, (detail, count))| format!("{kind:<17}{detail:<20}×{count}"))
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_monitor_records_nothing() {
        let m = MidiMonitor::new(false);
        m.record(&[0xD0, 100]);
        assert!(!m.is_enabled());
        assert!(m.snapshot().is_empty());
    }

    #[test]
    fn tallies_by_kind_and_counts() {
        let m = MidiMonitor::new(true);
        m.record(&[0xD0, 90]); // channel pressure
        m.record(&[0xD0, 110]); // channel pressure again → count 2, latest bytes update
        m.record(&[0xB0, 74, 20]); // CC 74
        m.record(&[0xE0, 0x00, 0x50]); // pitch bend

        let rows = m.snapshot();
        assert!(rows.iter().any(|r| r.contains("Channel Pressure") && r.contains("×2")), "{rows:?}");
        assert!(rows.iter().any(|r| r.starts_with("CC 74")), "{rows:?}");
        assert!(rows.iter().any(|r| r.contains("Pitch Bend")), "{rows:?}");
        // Latest channel-pressure bytes reflect the most recent message (0x6e = 110).
        assert!(
            rows.iter().any(|r| r.contains("Channel Pressure") && r.contains("6e")),
            "latest bytes should update: {rows:?}"
        );
    }
}
