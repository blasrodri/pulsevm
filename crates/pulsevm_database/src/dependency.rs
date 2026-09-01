//! Transaction-local database dependency recording.
//!
//! This is the observation-only first stage of optimistic execution. A
//! recorder is attached to a cloned [`crate::Database`] handle, so all clones
//! made for inline actions and WASM host functions share one transaction-local
//! report while unrelated transactions do not. Recording never participates in
//! a database result and is absent from the default execution path.

use std::{
    collections::BTreeSet,
    sync::{
        Arc,
        Mutex,
    },
};

/// A consensus contract-table index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ContractIndex {
    /// The table metadata row (`table_id_object`).
    Table,
    Primary,
    Idx64,
    Idx128,
    Idx256,
    IdxDouble,
    IdxLongDouble,
}

/// Stable logical identity of one contract database row.
///
/// Secondary rows are identified by their primary key. Their secondary value
/// can change, but the logical row (and its RAM payer) remains the same.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ContractRowKey {
    pub code: u64,
    pub scope: u64,
    pub table: u64,
    pub index: ContractIndex,
    pub primary: u64,
}

impl ContractRowKey {
    pub(crate) const fn new(
        code: u64,
        scope: u64,
        table: u64,
        index: ContractIndex,
        primary: u64,
    ) -> Self {
        Self {
            code,
            scope,
            table,
            index,
            primary,
        }
    }

    pub(crate) const fn table(code: u64, scope: u64, table: u64) -> Self {
        Self::new(code, scope, table, ContractIndex::Table, 0)
    }
}

/// A conservative dependency on ordering or absence within one whole index.
///
/// Lower/upper-bound and iterator steps can change when any row is inserted,
/// removed, or re-keyed in the index. Whole-index dependencies intentionally
/// over-report conflicts until interval phantom tracking is proven correct.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ContractRangeKey {
    pub code: u64,
    pub scope: u64,
    pub table: u64,
    pub index: ContractIndex,
}

impl ContractRangeKey {
    pub(crate) const fn new(code: u64, scope: u64, table: u64, index: ContractIndex) -> Self {
        Self {
            code,
            scope,
            table,
            index,
        }
    }
}

/// Dependencies observed while executing one serial transaction.
///
/// `complete` is deliberately false in this first slice: contract-table
/// intrinsics are covered, while permissions, resource limits, deferred
/// transactions, protocol state, and other system tables are not yet versioned.
/// An optimistic commit implementation must reject incomplete reports.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TransactionDependencies {
    exact_reads: BTreeSet<ContractRowKey>,
    range_reads: BTreeSet<ContractRangeKey>,
    writes: BTreeSet<ContractRowKey>,
    complete: bool,
}

impl TransactionDependencies {
    pub fn exact_reads(&self) -> &BTreeSet<ContractRowKey> {
        &self.exact_reads
    }

    pub fn range_reads(&self) -> &BTreeSet<ContractRangeKey> {
        &self.range_reads
    }

    pub fn writes(&self) -> &BTreeSet<ContractRowKey> {
        &self.writes
    }

    pub fn is_complete(&self) -> bool {
        self.complete
    }

    pub fn exact_read_count(&self) -> usize {
        self.exact_reads.len()
    }

    pub fn range_read_count(&self) -> usize {
        self.range_reads.len()
    }

    pub fn write_count(&self) -> usize {
        self.writes.len()
    }

    /// Whether earlier serial-order writes invalidate this observation.
    ///
    /// Writes are implicit reads: an update/remove must conflict if an earlier
    /// transaction changed the same logical row. A range read conflicts with
    /// any earlier write to that index, providing conservative phantom safety.
    /// Incomplete reports must still be rejected by the caller independently.
    pub fn conflicts_with_prior_writes(&self, prior_writes: &BTreeSet<ContractRowKey>) -> bool {
        prior_writes.iter().any(|write| {
            self.exact_reads.contains(write)
                || self.writes.contains(write)
                || self.range_reads.contains(&ContractRangeKey::new(
                    write.code,
                    write.scope,
                    write.table,
                    write.index,
                ))
        })
    }

    /// Safe ordered-commit gate for a future optimistic executor.
    ///
    /// Keeping the completeness check next to conflict validation prevents a
    /// partially instrumented report from being accidentally treated as valid.
    pub fn can_optimistically_commit_after(&self, prior_writes: &BTreeSet<ContractRowKey>) -> bool {
        self.complete && !self.conflicts_with_prior_writes(prior_writes)
    }
}

#[derive(Clone, Default)]
pub(crate) struct DependencyRecorder {
    inner: Arc<Mutex<TransactionDependencies>>,
}

impl DependencyRecorder {
    pub(crate) fn exact_read(&self, key: ContractRowKey) {
        // Telemetry must never affect consensus execution. A poisoned recorder
        // is therefore ignored rather than surfaced through a database API.
        if let Ok(mut report) = self.inner.lock() {
            report.exact_reads.insert(key);
        }
    }

    pub(crate) fn range_read(&self, key: ContractRangeKey) {
        if let Ok(mut report) = self.inner.lock() {
            report.range_reads.insert(key);
        }
    }

    pub(crate) fn write(&self, key: ContractRowKey) {
        if let Ok(mut report) = self.inner.lock() {
            report.writes.insert(key);
        }
    }

    fn snapshot(&self) -> TransactionDependencies {
        self.inner
            .lock()
            .map(|report| report.clone())
            .unwrap_or_default()
    }
}

/// Read handle for a transaction-local dependency report.
///
/// The handle can be sampled after execution even though the `Database` clone
/// carrying the recorder has moved through transaction and action contexts.
#[derive(Clone, Default)]
pub struct DependencyTracker {
    recorder: DependencyRecorder,
}

impl DependencyTracker {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub fn snapshot(&self) -> TransactionDependencies {
        self.recorder.snapshot()
    }

    pub(crate) fn recorder(&self) -> DependencyRecorder {
        self.recorder.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorder_deduplicates_stable_dependencies() {
        let tracker = DependencyTracker::new();
        let row = ContractRowKey::new(1, 2, 3, ContractIndex::Primary, 4);
        let range = ContractRangeKey::new(1, 2, 3, ContractIndex::Primary);

        tracker.recorder.exact_read(row);
        tracker.recorder.exact_read(row);
        tracker.recorder.range_read(range);
        tracker.recorder.range_read(range);
        tracker.recorder.write(row);
        tracker.recorder.write(row);

        let report = tracker.snapshot();
        assert_eq!(report.exact_reads, BTreeSet::from([row]));
        assert_eq!(report.range_reads, BTreeSet::from([range]));
        assert_eq!(report.writes, BTreeSet::from([row]));
        assert!(!report.is_complete());
    }

    #[test]
    fn cloned_trackers_share_only_their_own_report() {
        let first = DependencyTracker::new();
        let first_clone = first.clone();
        let second = DependencyTracker::new();
        let row = ContractRowKey::table(7, 8, 9);

        first_clone.recorder.write(row);

        assert_eq!(first.snapshot().writes, BTreeSet::from([row]));
        assert!(second.snapshot().writes.is_empty());
    }

    #[test]
    fn conflict_check_covers_exact_write_and_range_phantoms() {
        let table = (11, 12, 13);
        let row_7 = ContractRowKey::new(table.0, table.1, table.2, ContractIndex::Idx64, 7);
        let row_8 = ContractRowKey::new(table.0, table.1, table.2, ContractIndex::Idx64, 8);
        let unrelated = ContractRowKey::new(99, 12, 13, ContractIndex::Idx64, 8);

        let exact = TransactionDependencies {
            exact_reads: BTreeSet::from([row_7]),
            ..Default::default()
        };
        assert!(exact.conflicts_with_prior_writes(&BTreeSet::from([row_7])));
        assert!(!exact.conflicts_with_prior_writes(&BTreeSet::from([row_8])));
        assert!(!exact.can_optimistically_commit_after(&BTreeSet::new()));

        let complete_exact = TransactionDependencies {
            complete: true,
            ..exact.clone()
        };
        assert!(complete_exact.can_optimistically_commit_after(&BTreeSet::from([row_8])));
        assert!(!complete_exact.can_optimistically_commit_after(&BTreeSet::from([row_7])));

        let write = TransactionDependencies {
            writes: BTreeSet::from([row_7]),
            ..Default::default()
        };
        assert!(write.conflicts_with_prior_writes(&BTreeSet::from([row_7])));

        let range = TransactionDependencies {
            range_reads: BTreeSet::from([ContractRangeKey::new(
                table.0,
                table.1,
                table.2,
                ContractIndex::Idx64,
            )]),
            ..Default::default()
        };
        assert!(range.conflicts_with_prior_writes(&BTreeSet::from([row_8])));
        assert!(!range.conflicts_with_prior_writes(&BTreeSet::from([unrelated])));
    }
}
