use evm_chain::ChainAdapter;
use evm_core::{BlockNumber, ChainId};
use evm_protocol::{ProtocolAdapter, ProtocolEvent};
use evm_state::{InMemoryStateStore, StateError, StateSnapshot, StateStore, StateUpdate};

use crate::error::{ReplayError, Result};
use crate::pipeline::EventPipeline;

/// What a run did, in numbers the acceptance tests can assert on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReplayReport {
    pub chain_id: Option<ChainId>,
    pub from_block: Option<BlockNumber>,
    pub to_block: Option<BlockNumber>,
    pub blocks: u64,
    pub logs: u64,
    pub sync_events: u64,
    pub swap_events: u64,
    pub pool_created_events: u64,
    pub registrations: u64,
    pub syncs_applied: u64,
    /// Syncs the store refused because the reserves themselves are not a state
    /// — an empty side. Every other refusal stops the run instead.
    pub rejected_syncs: u64,
    /// Why each rejected sync was rejected, in the order they were met.
    pub rejections: Vec<String>,
    /// Pools the store knows (registered), and how many of them have reserves.
    pub pools: usize,
    pub synced_pools: usize,
}

/// Block range in, pool state out. The engine holds no chain-specific logic: it
/// asks a [`ChainAdapter`] for normalized blocks and the pipeline for updates,
/// which is what lets the same code serve replay and live.
pub struct ReplayEngine<A> {
    chain: A,
    pipeline: EventPipeline,
    store: InMemoryStateStore,
}

impl<A: ChainAdapter> ReplayEngine<A> {
    pub fn new(
        chain: A,
        protocol_adapters: Vec<Box<dyn ProtocolAdapter>>,
        store: InMemoryStateStore,
    ) -> Self {
        Self {
            chain,
            pipeline: EventPipeline::new(protocol_adapters),
            store,
        }
    }

    pub fn chain_id(&self) -> ChainId {
        self.chain.chain_id()
    }

    pub fn store(&self) -> &InMemoryStateStore {
        &self.store
    }

    pub fn snapshot(&self) -> StateSnapshot {
        self.store.snapshot()
    }

    /// Every block in the range must be available: a gap is an error, not a
    /// silently shorter run.
    pub async fn replay_range(
        &mut self,
        from: BlockNumber,
        to: BlockNumber,
    ) -> Result<ReplayReport> {
        if from > to {
            return Err(ReplayError::InvertedRange {
                from: from.0,
                to: to.0,
            });
        }
        let mut report = ReplayReport {
            chain_id: Some(self.chain_id()),
            from_block: Some(from),
            to_block: Some(to),
            ..ReplayReport::default()
        };
        for number in from.0..=to.0 {
            self.replay_block(BlockNumber(number), &mut report).await?;
        }
        Ok(report)
    }

    pub async fn replay_block(
        &mut self,
        number: BlockNumber,
        report: &mut ReplayReport,
    ) -> Result<()> {
        let data = self.chain.get_block_data(number).await?;
        report.blocks += 1;
        for log in data.ordered_logs() {
            report.logs += 1;
            let Some(event) = self.pipeline.decode(log)? else {
                continue;
            };
            match &event {
                ProtocolEvent::Sync(_) => report.sync_events += 1,
                ProtocolEvent::Swap(_) => report.swap_events += 1,
                ProtocolEvent::PoolCreated(_) => report.pool_created_events += 1,
            }
            for update in self
                .pipeline
                .updates_for(&event, self.store.pool_meta(event.pool()))
            {
                let is_sync = matches!(update, StateUpdate::PoolSynced { .. });
                if let Err(err) = self.store.apply(update) {
                    // An empty reserve side is a fact about the market, not a
                    // fault in this run: refuse that one statement, keep the
                    // pool's last valid state, and record why. Every other
                    // rejection means the run itself cannot be trusted, so it
                    // stops here rather than quietly continuing.
                    if matches!(&err, StateError::InvalidReserves { .. }) {
                        report.rejected_syncs += 1;
                        report.rejections.push(err.to_string());
                        continue;
                    }
                    return Err(err.into());
                }
                if is_sync {
                    report.syncs_applied += 1;
                } else {
                    report.registrations += 1;
                }
            }
        }
        report.pools = self.store.pool_count();
        report.synced_pools = self.store.synced_pool_count();
        Ok(())
    }
}
