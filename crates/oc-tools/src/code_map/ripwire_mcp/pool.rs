//! Bounded warm-daemon pool: binary/root fingerprinting, LRU + TTL eviction and
//! the lazy per-runtime idle reaper.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::{Duration, Instant, UNIX_EPOCH};

use tokio::sync::Mutex as AsyncMutex;

use super::daemon::McpDaemon;

/// Maximum concurrent warm `ripwire --mcp` daemons across workspaces.
const MAX_DAEMONS: usize = 2;

/// Idle eviction threshold for unused `ripwire --mcp` daemons.
const DAEMON_IDLE_TTL: Duration = Duration::from_secs(300);

/// Periodic check interval for the lazy idle reaper task.
const REAPER_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct BinaryFingerprint {
    path: PathBuf,
    len: u64,
    modified_nanos: Option<u128>,
}

impl BinaryFingerprint {
    fn of(path: &Path) -> Self {
        let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        match fs::metadata(&canonical) {
            Ok(meta) => {
                let modified_nanos = meta
                    .modified()
                    .ok()
                    .and_then(|ts| ts.duration_since(UNIX_EPOCH).ok())
                    .map(|dur| dur.as_nanos());
                Self {
                    path: canonical,
                    len: meta.len(),
                    modified_nanos,
                }
            }
            Err(_) => Self {
                path: canonical,
                len: 0,
                modified_nanos: None,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PoolKey {
    runtime_id: tokio::runtime::Id,
    binary: BinaryFingerprint,
    root: PathBuf,
    scip: Option<BinaryFingerprint>,
}

#[derive(Debug)]
pub(super) struct PoolSlot {
    pub(super) daemon: AsyncMutex<Option<McpDaemon>>,
    last_used: StdMutex<Instant>,
}

impl PoolSlot {
    fn new() -> Self {
        Self {
            daemon: AsyncMutex::new(None),
            last_used: StdMutex::new(Instant::now()),
        }
    }

    pub(super) fn touch(&self) {
        *self
            .last_used
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Instant::now();
    }

    fn idle_elapsed(&self, now: Instant) -> Duration {
        let last = *self
            .last_used
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        now.saturating_duration_since(last)
    }
}

#[derive(Debug, Default)]
struct PoolState {
    slots: HashMap<PoolKey, Arc<PoolSlot>>,
    cli_only_binaries: HashSet<BinaryFingerprint>,
    reaper_runtimes: HashSet<tokio::runtime::Id>,
}

fn global_pool() -> &'static StdMutex<PoolState> {
    static POOL: OnceLock<StdMutex<PoolState>> = OnceLock::new();
    POOL.get_or_init(|| StdMutex::new(PoolState::default()))
}

fn canonical_root(root: &Path) -> PathBuf {
    fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf())
}

fn ensure_reaper_for_runtime(runtime: &tokio::runtime::Handle, pool: &mut PoolState) {
    let runtime_id = runtime.id();
    if pool.reaper_runtimes.contains(&runtime_id) {
        return;
    }
    pool.reaper_runtimes.insert(runtime_id);
    runtime.spawn(async move {
        loop {
            tokio::time::sleep(REAPER_INTERVAL).await;
            let now = Instant::now();
            let to_drop = {
                let mut state = global_pool()
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let mut evicted = Vec::new();
                state.slots.retain(|key, slot| {
                    if key.runtime_id != runtime_id {
                        return true;
                    }
                    if slot.idle_elapsed(now) >= DAEMON_IDLE_TTL {
                        // Only evict idle slots whose daemon lock is currently free.
                        if let Ok(mut guard) = slot.daemon.try_lock() {
                            if let Some(daemon) = guard.take() {
                                evicted.push(daemon);
                            }
                            return false;
                        }
                    }
                    true
                });
                let still_has_entries = state.slots.keys().any(|key| key.runtime_id == runtime_id);
                if !still_has_entries {
                    state.reaper_runtimes.remove(&runtime_id);
                }
                (evicted, !still_has_entries)
            };
            drop(to_drop.0);
            if to_drop.1 {
                break;
            }
        }
    });
}

pub(super) fn acquire_slot(
    binary: &Path,
    root: &Path,
    scip_path: Option<&Path>,
) -> Result<(BinaryFingerprint, Arc<PoolSlot>), Vec<u8>> {
    let fingerprint = BinaryFingerprint::of(binary);
    let runtime = tokio::runtime::Handle::current();
    let key = PoolKey {
        runtime_id: runtime.id(),
        binary: fingerprint.clone(),
        root: canonical_root(root),
        scip: scip_path.map(BinaryFingerprint::of),
    };

    let evicted_slots: Vec<Arc<PoolSlot>>;
    let slot = {
        let mut state = global_pool()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.cli_only_binaries.contains(&fingerprint) {
            return Err(Vec::new());
        }

        let now = Instant::now();
        let mut removed = Vec::new();
        // Reclaim stale slots idle past TTL whenever capacity is checked.
        state.slots.retain(|existing_key, existing_slot| {
            if existing_key != &key && existing_slot.idle_elapsed(now) >= DAEMON_IDLE_TTL {
                removed.push(Arc::clone(existing_slot));
                return false;
            }
            true
        });

        let slot = if let Some(existing) = state.slots.get(&key) {
            Arc::clone(existing)
        } else {
            // Bound active warm daemons per runtime to MAX_DAEMONS using LRU eviction,
            // never evicting a slot that is currently executing a call.
            loop {
                let same_runtime_count = state
                    .slots
                    .keys()
                    .filter(|k| k.runtime_id == key.runtime_id)
                    .count();
                if same_runtime_count < MAX_DAEMONS {
                    break;
                }
                let Some(oldest_key) = state
                    .slots
                    .iter()
                    .filter(|(k, s)| k.runtime_id == key.runtime_id && s.daemon.try_lock().is_ok())
                    .min_by_key(|(_, s)| {
                        *s.last_used
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                    })
                    .map(|(k, _)| k.clone())
                else {
                    break;
                };
                if let Some(evicted) = state.slots.remove(&oldest_key) {
                    removed.push(evicted);
                }
            }
            // Also keep total slots bounded across exited test runtimes.
            while state.slots.len() >= 16 {
                let Some(oldest_key) = state
                    .slots
                    .iter()
                    .filter(|(k, s)| k.runtime_id != key.runtime_id && s.daemon.try_lock().is_ok())
                    .min_by_key(|(_, s)| {
                        *s.last_used
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                    })
                    .map(|(k, _)| k.clone())
                else {
                    break;
                };
                if let Some(evicted) = state.slots.remove(&oldest_key) {
                    removed.push(evicted);
                }
            }
            let created = Arc::new(PoolSlot::new());
            state.slots.insert(key, Arc::clone(&created));
            created
        };

        ensure_reaper_for_runtime(&runtime, &mut state);
        evicted_slots = removed;
        slot
    };

    for evicted in evicted_slots {
        if let Ok(mut guard) = evicted.daemon.try_lock() {
            guard.take();
        }
    }

    slot.touch();
    Ok((fingerprint, slot))
}

pub(super) fn mark_cli_only(fingerprint: BinaryFingerprint, root: &Path, scip_path: Option<&Path>) {
    let runtime_id = tokio::runtime::Handle::current().id();
    let key = PoolKey {
        runtime_id,
        binary: fingerprint.clone(),
        root: canonical_root(root),
        scip: scip_path.map(BinaryFingerprint::of),
    };
    let mut state = global_pool()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if state.cli_only_binaries.len() >= 16 {
        state.cli_only_binaries.clear();
    }
    state.cli_only_binaries.insert(fingerprint);
    state.slots.remove(&key);
}
