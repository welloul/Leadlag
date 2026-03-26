//! Telemetry writer for Proto3 binary files.
//!
//! Writes raw ticks and lead-lag offset logs to binary files for replay.
//! Uses a dedicated background thread to avoid blocking the hot path.

use crate::eal::{Tick, VenueId};
use crossbeam_channel::Receiver;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Telemetry entry types.
#[derive(Debug, Clone)]
pub enum TelemetryEntry {
    /// Raw tick data.
    Tick(Tick),
    /// Lead-lag offset log.
    LeadLagOffset {
        timestamp_ns: u64,
        venue_a: VenueId,
        venue_b: VenueId,
        correlation: f64,
        lag_offset_ns: i64,
        lead_venue: VenueId,
    },
    /// Signal generated.
    Signal {
        timestamp_ns: u64,
        symbol: String,
        side: String,
        correlation: f64,
        lag_offset_ns: i64,
    },
}

/// Telemetry writer.
///
/// Writes telemetry entries to binary files in the background.
pub struct TelemetryWriter {
    /// Sender for telemetry entries.
    sender: crossbeam_channel::Sender<TelemetryEntry>,
    /// Shutdown flag.
    shutdown: Arc<AtomicBool>,
    /// Writer thread handle.
    handle: Option<thread::JoinHandle<()>>,
}

impl TelemetryWriter {
    /// Create a new telemetry writer.
    ///
    /// Spawns a background thread that writes entries to disk.
    pub fn new(base_path: &str) -> Result<Self, std::io::Error> {
        let (tx, rx) = crossbeam_channel::bounded(10_000);
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = shutdown.clone();

        let base_path = PathBuf::from(base_path);
        std::fs::create_dir_all(&base_path)?;

        let handle = thread::spawn(move || {
            Self::writer_loop(rx, shutdown_clone, base_path);
        });

        Ok(Self {
            sender: tx,
            shutdown,
            handle: Some(handle),
        })
    }

    /// Writer loop (runs in background thread).
    fn writer_loop(
        rx: Receiver<TelemetryEntry>,
        shutdown: Arc<AtomicBool>,
        base_path: PathBuf,
    ) {
        let mut current_file = Self::open_new_file(&base_path);
        let mut entries_in_file = 0;
        let max_entries_per_file = 100_000;

        loop {
            // Check shutdown flag
            if shutdown.load(Ordering::Relaxed) {
                break;
            }

            // Try to receive entries (non-blocking)
            match rx.try_recv() {
                Ok(entry) => {
                    // Write entry
                    if let Err(e) = Self::write_entry(&mut current_file, &entry) {
                        eprintln!("Telemetry write error: {e}");
                    }
                    entries_in_file += 1;

                    // Rotate file if needed
                    if entries_in_file >= max_entries_per_file {
                        current_file = Self::open_new_file(&base_path);
                        entries_in_file = 0;
                    }
                }
                Err(crossbeam_channel::TryRecvError::Empty) => {
                    // No entries, flush and sleep
                    let _ = current_file.flush();
                    thread::sleep(Duration::from_millis(100));
                }
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    break;
                }
            }
        }

        // Final flush
        let _ = current_file.flush();
    }

    /// Open a new telemetry file.
    fn open_new_file(base_path: &Path) -> BufWriter<File> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let filename = format!("telemetry_{timestamp}.bin");
        let path = base_path.join(filename);

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("Failed to open telemetry file");

        BufWriter::with_capacity(128 * 1024, file) // 128KB buffer
    }

    /// Write a telemetry entry to the file.
    fn write_entry(writer: &mut BufWriter<File>, entry: &TelemetryEntry) -> std::io::Result<()> {
        match entry {
            TelemetryEntry::Tick(tick) => {
                // Simple binary format: type(1) + venue(1) + price(8) + size(8) + ts(8)
                writer.write_all(&[0x01])?; // Type: Tick
                writer.write_all(&[tick.venue.0])?;
                writer.write_all(&tick.price.to_le_bytes())?;
                writer.write_all(&tick.size.to_le_bytes())?;
                writer.write_all(&tick.exchange_ts_ns.to_le_bytes())?;
            }
            TelemetryEntry::LeadLagOffset {
                timestamp_ns,
                correlation,
                lag_offset_ns,
                lead_venue,
                ..
            } => {
                // Type: LeadLagOffset
                writer.write_all(&[0x02])?;
                writer.write_all(&timestamp_ns.to_le_bytes())?;
                writer.write_all(&correlation.to_le_bytes())?;
                writer.write_all(&lag_offset_ns.to_le_bytes())?;
                writer.write_all(&[lead_venue.0])?;
            }
            TelemetryEntry::Signal {
                timestamp_ns,
                symbol,
                side,
                correlation,
                lag_offset_ns,
            } => {
                // Type: Signal
                writer.write_all(&[0x03])?;
                writer.write_all(&timestamp_ns.to_le_bytes())?;
                writer.write_all(&correlation.to_le_bytes())?;
                writer.write_all(&lag_offset_ns.to_le_bytes())?;
                // Write symbol length + bytes
                let symbol_bytes = symbol.as_bytes();
                writer.write_all(&(symbol_bytes.len() as u16).to_le_bytes())?;
                writer.write_all(symbol_bytes)?;
                // Write side length + bytes
                let side_bytes = side.as_bytes();
                writer.write_all(&(side_bytes.len() as u16).to_le_bytes())?;
                writer.write_all(side_bytes)?;
            }
        }
        Ok(())
    }

    /// Log a tick.
    pub fn log_tick(&self, tick: &Tick) {
        let _ = self.sender.try_send(TelemetryEntry::Tick(tick.clone()));
    }

    /// Log a lead-lag offset.
    pub fn log_lead_lag(
        &self,
        venue_a: VenueId,
        venue_b: VenueId,
        correlation: f64,
        lag_offset_ns: i64,
        lead_venue: VenueId,
    ) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;

        let _ = self.sender.try_send(TelemetryEntry::LeadLagOffset {
            timestamp_ns: now,
            venue_a,
            venue_b,
            correlation,
            lag_offset_ns,
            lead_venue,
        });
    }

    /// Log a signal.
    pub fn log_signal(
        &self,
        symbol: &str,
        side: &str,
        correlation: f64,
        lag_offset_ns: i64,
    ) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;

        let _ = self.sender.try_send(TelemetryEntry::Signal {
            timestamp_ns: now,
            symbol: symbol.to_string(),
            side: side.to_string(),
            correlation,
            lag_offset_ns,
        });
    }

    /// Shutdown the writer.
    pub fn shutdown(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for TelemetryWriter {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_telemetry_writer_creation() {
        let dir = tempdir().unwrap();
        let writer = TelemetryWriter::new(dir.path().to_str().unwrap());
        assert!(writer.is_ok());
    }
}