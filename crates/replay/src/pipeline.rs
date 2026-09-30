use evm_chain::ChainLog;
use evm_core::{PoolId, PoolMeta};
use evm_protocol::{ProtocolAdapter, ProtocolEvent};
use evm_state::{StateUpdate, UpdatePosition};

/// The one place protocol events become state updates.
///
/// Keeping this separate matters: the decoder says what a log means, this says
/// what may be written because of it, and the store says whether that write is
/// admissible. Live and replay both come through here.
pub struct EventPipeline {
    adapters: Vec<Box<dyn ProtocolAdapter>>,
}

impl EventPipeline {
    pub fn new(adapters: Vec<Box<dyn ProtocolAdapter>>) -> Self {
        Self { adapters }
    }

    pub fn adapter_count(&self) -> usize {
        self.adapters.len()
    }

    /// The first adapter that claims a log owns it. A log no adapter claims is
    /// not an error — most logs on a chain are nobody's business.
    pub fn decode(&self, log: &ChainLog) -> crate::Result<Option<ProtocolEvent>> {
        for adapter in &self.adapters {
            if let Some(event) = adapter.decode_log(log)? {
                return Ok(Some(event));
            }
        }
        Ok(None)
    }

    /// Registration comes from attested metadata, never from the event itself: a
    /// log can state reserves, it cannot promote an address to a pool.
    pub fn updates_for(
        &self,
        event: &ProtocolEvent,
        registered: Option<&PoolMeta>,
    ) -> Vec<StateUpdate> {
        let mut updates = Vec::new();
        if registered.is_none() {
            if let Some(meta) = self.meta_for(event.pool()) {
                updates.push(StateUpdate::PoolRegistered(meta));
            }
        }
        match event {
            ProtocolEvent::Sync(sync) => updates.push(StateUpdate::PoolSynced {
                pool: sync.pool,
                reserve0: sync.reserve0,
                reserve1: sync.reserve1,
                position: UpdatePosition::new(sync.position.block_number, sync.position.log_index),
            }),
            // A swap is flow. It never produces reserves, so it never reaches
            // the store at all.
            ProtocolEvent::Swap(_) | ProtocolEvent::PoolCreated(_) => {}
        }
        updates
    }

    fn meta_for(&self, pool: PoolId) -> Option<PoolMeta> {
        self.adapters.iter().find_map(|adapter| {
            adapter
                .attestation(pool)
                .map(|attestation| attestation.to_meta())
        })
    }
}
