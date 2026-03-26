//! State store using Sled embedded database.
//!
//! Persists positions, nonces, and daily PnL for crash recovery.
//! Uses optimistic writes to avoid blocking the OMS.

use sled::Db;
use std::path::Path;
use std::sync::Arc;

/// State store for persistent state.
///
/// Uses Sled embedded database for fast key-value storage.
pub struct StateStore {
    /// Sled database instance.
    db: Arc<Db>,
}

impl StateStore {
    /// Open or create a state store at the given path.
    pub fn open(path: &str) -> Result<Self, sled::Error> {
        let db = sled::open(path)?;
        Ok(Self { db: Arc::new(db) })
    }

    /// Store a position.
    pub fn store_position(
        &self,
        venue_id: u8,
        symbol: &str,
        size: f64,
        entry_price: f64,
    ) -> Result<(), sled::Error> {
        let key = format!("pos:{venue_id}:{symbol}");
        let value = format!("{size}:{entry_price}");
        self.db.insert(key.as_bytes(), value.as_bytes())?;
        Ok(())
    }

    /// Load a position.
    pub fn load_position(
        &self,
        venue_id: u8,
        symbol: &str,
    ) -> Result<Option<(f64, f64)>, sled::Error> {
        let key = format!("pos:{venue_id}:{symbol}");
        match self.db.get(key.as_bytes())? {
            Some(value) => {
                let value_str = String::from_utf8_lossy(&value);
                let parts: Vec<&str> = value_str.split(':').collect();
                if parts.len() == 2 {
                    let size = parts[0].parse::<f64>().unwrap_or(0.0);
                    let entry_price = parts[1].parse::<f64>().unwrap_or(0.0);
                    Ok(Some((size, entry_price)))
                } else {
                    Ok(None)
                }
            }
            None => Ok(None),
        }
    }

    /// Store daily realized PnL.
    pub fn store_daily_pnl(&self, pnl: f64) -> Result<(), sled::Error> {
        self.db.insert(b"daily_pnl", pnl.to_le_bytes().to_vec())?;
        Ok(())
    }

    /// Load daily realized PnL.
    pub fn load_daily_pnl(&self) -> Result<f64, sled::Error> {
        match self.db.get(b"daily_pnl")? {
            Some(value) => {
                let bytes: [u8; 8] = value.as_ref().try_into().unwrap_or([0; 8]);
                Ok(f64::from_le_bytes(bytes))
            }
            None => Ok(0.0),
        }
    }

    /// Store nonce for a venue.
    pub fn store_nonce(&self, venue_id: u8, nonce: u64) -> Result<(), sled::Error> {
        let key = format!("nonce:{venue_id}");
        self.db.insert(key.as_bytes(), nonce.to_le_bytes().to_vec())?;
        Ok(())
    }

    /// Load nonce for a venue.
    pub fn load_nonce(&self, venue_id: u8) -> Result<u64, sled::Error> {
        let key = format!("nonce:{venue_id}");
        match self.db.get(key.as_bytes())? {
            Some(value) => {
                let bytes: [u8; 8] = value.as_ref().try_into().unwrap_or([0; 8]);
                Ok(u64::from_le_bytes(bytes))
            }
            None => Ok(0),
        }
    }

    /// Flush all pending writes to disk.
    pub fn flush(&self) -> Result<usize, sled::Error> {
        self.db.flush()
    }

    /// Clear all state (for testing).
    pub fn clear(&self) -> Result<(), sled::Error> {
        self.db.clear()?;
        Ok(())
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
    fn test_state_store_position() {
        let dir = tempdir().unwrap();
        let store = StateStore::open(dir.path().to_str().unwrap()).unwrap();

        store.store_position(0, "BTC", 0.5, 60000.0).unwrap();
        let (size, entry) = store.load_position(0, "BTC").unwrap().unwrap();

        assert_eq!(size, 0.5);
        assert_eq!(entry, 60000.0);
    }

    #[test]
    fn test_state_store_daily_pnl() {
        let dir = tempdir().unwrap();
        let store = StateStore::open(dir.path().to_str().unwrap()).unwrap();

        store.store_daily_pnl(123.45).unwrap();
        let pnl = store.load_daily_pnl().unwrap();

        assert!((pnl - 123.45).abs() < 1e-6);
    }

    #[test]
    fn test_state_store_nonce() {
        let dir = tempdir().unwrap();
        let store = StateStore::open(dir.path().to_str().unwrap()).unwrap();

        store.store_nonce(0, 42).unwrap();
        let nonce = store.load_nonce(0).unwrap();

        assert_eq!(nonce, 42);
    }
}