//! §34's other half: the refusals that come from the state source rather than from
//! the EVM. A run can fail to answer for three reasons that say different things
//! about the provider — a contract that is not there, a piece of state that is not
//! there, and a provider that did not reply — and the milestone is not allowed to
//! collapse them, because the first says "this route cannot execute", the second
//! says "this block cannot be simulated", and the third says "ask again".
//!
//! Each test withholds exactly one kind of answer from the fixture provider and
//! leaves everything else passing straight through, including the source string the
//! §20 check compares. That transparency is what makes the variant the finding: a
//! wrapper that also changed the pin or the source would be reporting
//! [`StateMismatch`][SimulationError::StateMismatch] instead, and the test would pass
//! for the wrong reason. Every one of them runs against the same request that the
//! control completes two steps earlier.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use alloy_primitives::{Address, Bytes, B256, U256};
use async_trait::async_trait;

use evm_chain::BlockContext;
use evm_core::{BlockNumber, ChainId};
use evm_simulation::{
    engine::run, AccountState, BlockPin, DumpStateProvider, EvmRules, ProviderError,
    ProviderResult, SimulationError, SimulationRequest, StateOverride, StateProvider,
    TransactionSpec,
};

mod support;
use support::{request, route};

/// What the wrapper refuses to give, and nothing else.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Withhold {
    /// `eth_getCode` answers with no bytes for this one address (§60).
    CodeOf(Address),
    /// Every storage read reports that the state is not there.
    Storage,
    /// Every account read fails as the *provider* failing, which is a different
    /// answer from "the state is absent" and becomes a different variant.
    AccountUnavailable,
}

struct Withholding {
    inner: Arc<dyn StateProvider>,
    withholding: Withhold,
    /// Reads that reached this wrapper. §60 is an ordering claim as much as a
    /// variant claim: empty code has to stop the run while it is still asking
    /// permission, not after it has started executing and read slots on the way.
    reads: Arc<AtomicUsize>,
}

impl Withholding {
    fn wrap(inner: Arc<DumpStateProvider>, withholding: Withhold) -> Arc<Self> {
        Arc::new(Self {
            inner,
            withholding,
            reads: Arc::new(AtomicUsize::new(0)),
        })
    }

    fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl StateProvider for Withholding {
    fn chain_id(&self) -> ChainId {
        self.inner.chain_id()
    }

    fn pin(&self) -> BlockPin {
        self.inner.pin()
    }

    fn source(&self) -> String {
        self.inner.source()
    }

    async fn account(&self, address: Address) -> ProviderResult<Option<AccountState>> {
        if self.withholding == Withhold::AccountUnavailable {
            return Err(ProviderError::Unavailable {
                provider: self.source(),
                reason: format!("the account read for {address} was refused by the test"),
            });
        }
        self.inner.account(address).await
    }

    async fn storage(&self, address: Address, slot: U256) -> ProviderResult<U256> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if self.withholding == Withhold::Storage {
            return Err(ProviderError::Missing {
                provider: self.source(),
                what: format!("storage of {address} at slot {slot}"),
            });
        }
        self.inner.storage(address, slot).await
    }

    async fn code(&self, address: Address) -> ProviderResult<Bytes> {
        if self.withholding == Withhold::CodeOf(address) {
            return Ok(Bytes::new());
        }
        self.inner.code(address).await
    }

    async fn header(&self) -> ProviderResult<BlockContext> {
        self.inner.header().await
    }

    async fn block_hash(&self, number: BlockNumber) -> ProviderResult<Option<B256>> {
        self.inner.block_hash(number).await
    }

    fn with_setup(&self, overrides: Vec<StateOverride>) -> Arc<dyn StateProvider> {
        Arc::new(Self {
            inner: self.inner.with_setup(overrides),
            withholding: self.withholding,
            // Shared: the derived provider is the same reader, and a counter that
            // reset at the setup boundary would hide the reads that matter.
            reads: Arc::clone(&self.reads),
        })
    }
}

/// One route, one block, one ask — the same request every test here uses, so the
/// only variable is what the provider answers.
struct Stages {
    provider: Arc<DumpStateProvider>,
    request: SimulationRequest,
}

impl Stages {
    async fn load() -> Self {
        let path = support::dump_path();
        assert!(
            path.exists(),
            "{} is missing. It is written by the live run; see dump_replay.rs.",
            path.display()
        );
        let provider = Arc::new(
            DumpStateProvider::from_file(&path)
                .unwrap_or_else(|error| panic!("{}: {error}", path.display())),
        );
        let opportunity = support::opportunity().await;
        let priced = route(&opportunity);
        let header = provider
            .header()
            .await
            .expect("the fixture carries the header it was read under");
        let source = provider.source();
        let ask = priced.analytical_output;
        Self {
            provider,
            request: request(priced, header, ask, source),
        }
    }

    fn contract(&self, which: usize) -> Address {
        let touched = self.request.route.touched_contracts();
        *touched
            .get(which)
            .unwrap_or_else(|| panic!("this route touches {touched:?}"))
    }

    /// The same request, the same provider, nothing withheld: the control that says
    /// the refusal below is about the withheld answer and not about the request.
    async fn control(&self) -> Result<(), SimulationError> {
        run(self.shared(), &self.request).await.map(|_| ())
    }

    fn shared(&self) -> Arc<dyn StateProvider> {
        self.provider.clone()
    }

    /// The run's request with the transaction shape changed and nothing else.
    fn request_with(&self, change: impl FnOnce(&mut TransactionSpec)) -> SimulationRequest {
        let mut request = self.request.clone();
        change(&mut request.transaction);
        request
    }
}

/// §60: an empty `eth_getCode` is a refusal to simulate, and it arrives before the
/// sequence reads a single slot — not as a zero balance, not as a step that called
/// an empty address and "succeeded" by doing nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_contract_with_no_code_stops_the_run_before_it_reads_any_state() {
    let stages = Stages::load().await;
    // The ask here is the analytical one, which the pools do not meet, so the
    // control's answer is a completed run that *reverts* — still an execution, and
    // still proof the request is not what makes the tests below fail.
    assert!(
        stages.control().await.is_ok(),
        "the request runs when nothing is withheld"
    );

    // The second contract the route touches: withholding the first would let a
    // refusal name the wrong address and still look like the check working.
    let address = stages.contract(1);
    let spy = Withholding::wrap(Arc::clone(&stages.provider), Withhold::CodeOf(address));
    let error = run(spy.clone() as Arc<dyn StateProvider>, &stages.request)
        .await
        .expect_err("a route whose contract has no code cannot be simulated");
    match error {
        SimulationError::MissingCode {
            address: named,
            block,
        } => {
            assert_eq!(named, address, "the refusal names the empty account");
            assert_eq!(
                block, stages.request.route.block_number,
                "and says which block it looked at"
            );
        }
        other => panic!("expected a missing-code refusal, got {other}"),
    }
    assert_eq!(
        spy.reads(),
        0,
        "the run stopped at the permission check; a code refusal that came after {} \
         storage reads would mean a step had already executed against the empty account",
        spy.reads(),
    );
}

/// §34: state that is not there is its own answer. The provider is working, the
/// header is readable, and the piece of state simply is not in the record — which is
/// neither a node failure nor a route that does not execute.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn storage_that_is_not_there_is_reported_as_missing_state() {
    let stages = Stages::load().await;
    assert!(
        stages.control().await.is_ok(),
        "the request runs when nothing is withheld"
    );

    let spy = Withholding::wrap(Arc::clone(&stages.provider), Withhold::Storage);
    let error = run(spy as Arc<dyn StateProvider>, &stages.request)
        .await
        .expect_err("a run with no storage answers cannot execute");
    match error {
        SimulationError::MissingState(what) => {
            println!("refused for missing state: {what}");
            assert!(
                what.contains("storage of"),
                "the message says which piece of state was wanted: {what}"
            );
        }
        SimulationError::ProviderError(why) => panic!(
            "state that is absent was reported as a provider failure, which is the merge \
             §34 forbids: {why}"
        ),
        other => panic!("expected a missing-state refusal, got {other}"),
    }
}

/// §34 read against §62: a provider that did not answer is the fourth kind of
/// answer, and it must not borrow the words of the third. Nothing was learned about
/// the state here — only about the node — so a reader cannot be told the route is
/// unexecutable.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_provider_that_does_not_reply_is_not_reported_as_missing_state() {
    let stages = Stages::load().await;
    assert!(
        stages.control().await.is_ok(),
        "the request runs when nothing is withheld"
    );

    let spy = Withholding::wrap(Arc::clone(&stages.provider), Withhold::AccountUnavailable);
    let error = run(spy as Arc<dyn StateProvider>, &stages.request)
        .await
        .expect_err("a provider that refuses every account read cannot carry a run");
    match error {
        SimulationError::ProviderError(why) => {
            println!("refused for a provider failure: {why}");
            assert!(
                why.contains("refused by the test"),
                "the reason travels with the variant: {why}"
            );
        }
        SimulationError::MissingState(what) => panic!(
            "a node that did not answer was reported as absent state, which tells the \
             reader the block cannot be simulated: {what}"
        ),
        other => panic!("expected a provider error, got {other}"),
    }
}

/// §34's last two, taken through the path that runs them rather than through the
/// builder that checks them: a transaction shape the declared ruleset refuses, and
/// one the pinned block's own gas limit refuses. These two belong with the three
/// above because all five are the engine saying "this never started" — and the point
/// of keeping them apart is that a reader can tell whether the fault is in this
/// crate's rules (§44's ceiling), in the block, or in the contracts.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_transaction_shape_the_rules_or_the_block_refuses_stops_the_run() {
    let stages = Stages::load().await;
    assert!(
        stages.control().await.is_ok(),
        "the request runs when its shape is the one the plan was built for"
    );

    // (a) The rules are declared, not derived, and one declared ruleset puts a
    // ceiling on a single transaction's gas limit that this plan's allowance is
    // nowhere near.
    let capped = stages.request_with(|spec| spec.rules = EvmRules::Osaka);
    let cap = EvmRules::Osaka.gas_limit_cap().expect("Osaka declares one");
    assert!(
        capped.transaction.gas_limit_per_step > cap,
        "this case is only about the cap if the plan is over it: {} <= {cap}",
        capped.transaction.gas_limit_per_step,
    );
    let error = run(stages.shared(), &capped)
        .await
        .expect_err("a step allowed more gas than the declared ruleset permits");
    match error {
        SimulationError::UnsupportedTransaction(reason) => {
            println!("the declared ruleset refused: {reason}");
            assert!(
                reason.contains(&cap.to_string()),
                "the refusal quotes the ceiling it applied: {reason}"
            );
        }
        other => panic!("expected an unsupported-transaction refusal, got {other}"),
    }

    // (b) The block is the other ceiling, and it comes from the pinned header rather
    // than from a ruleset: no transaction in this block could have been given more
    // than the header says, so a plan that asks for more never executes a step.
    let block_limit = stages.request.block.gas_limit;
    let asked = block_limit * 2;
    let oversized = stages.request_with(|spec| spec.gas_limit_per_step = asked);
    let error = run(stages.shared(), &oversized)
        .await
        .expect_err("a step allowed more gas than the block carries");
    match error {
        SimulationError::InvalidTransaction(reason) => {
            println!("the block refused {asked}: {reason}");
            assert!(
                reason.contains("block gas limit"),
                "the refusal names the rule it applied, against a header limit of \
                 {block_limit}: {reason}"
            );
        }
        SimulationError::UnsupportedTransaction(reason) => panic!(
            "a step over the block gas limit was reported as something this crate cannot \
             express, which is the merge §34 forbids: {reason}"
        ),
        other => panic!("expected an invalid-transaction refusal, got {other}"),
    }
}

/// §34 as a table: the same request, four different deprivations, four different
/// variants, and none of them the same string. Printed because the report quotes it
/// and the assertions above are what make it true.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn the_three_refusals_are_three_different_answers() {
    let stages = Stages::load().await;
    let address = stages.contract(1);
    let cases: Vec<(&str, Withhold)> = vec![
        ("no code", Withhold::CodeOf(address)),
        ("no storage", Withhold::Storage),
        ("no provider", Withhold::AccountUnavailable),
    ];
    let mut seen = Vec::new();
    for (label, withholding) in cases {
        let spy = Withholding::wrap(Arc::clone(&stages.provider), withholding);
        let error = run(spy as Arc<dyn StateProvider>, &stages.request)
            .await
            .expect_err("each deprivation stops the run");
        println!("{label:>12} -> {error}");
        let name = match error {
            SimulationError::MissingCode { .. } => "MissingCode",
            SimulationError::MissingState(_) => "MissingState",
            SimulationError::ProviderError(_) => "ProviderError",
            other => panic!("{label} was refused as {other}, which is not one of the three"),
        };
        seen.push(name);
    }
    assert_eq!(
        seen,
        vec!["MissingCode", "MissingState", "ProviderError"],
        "three deprivations, three variants"
    );
}
