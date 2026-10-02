//! M8.3.3 §9: one simulation's bounded state-read dispatch.
//!
//! The question this milestone asks is whether several pinned state reads can be
//! outstanding at the node at once without changing what the simulation answers.
//! §9 rules out the two easy ways to find out: `join_all` over a whole simulation
//! (unbounded — a read count the EVM demands is not a plan) and a pool, a batch
//! endpoint or a prefetch (all §3's forbidden list). What is left is this: a
//! scheduler owned by one [`RpcStateProvider`][crate::state::RpcStateProvider],
//! given a hand of already-decided reads, running no more than `limit` of them at
//! a time.
//!
//! Two properties hold the experiment honest, and both are this type's job:
//!
//! - **`limit == 1` is the baseline, not an approximation of it.** Tasks are
//!   awaited one after another in input order and the results come back in input
//!   order, which is what the sequential statements this replaced did. §10's check
//!   that concurrency 1 reproduces M8.3.2's 39-call run is a test, not an
//!   assumption.
//! - **A chunk finishes before the batch stops.** When one read of a chunk fails,
//!   its siblings have already been sent to the node; returning at the first error
//!   would drop the futures of calls that physically happened and leave the trace
//!   with no event for them. §16 counts what the wire carried, so a chunk is awaited
//!   in full, the caller reports the *first* failure by input order, and no later
//!   chunk starts. A failing batch therefore asks the node for at most one chunk's
//!   worth of reads the serial arm would not have made — a difference from the
//!   baseline worth reporting, not worth hiding.
//!
//! Nothing here decides *which* reads may share a batch — that is
//! [`crate::state`]'s dependency map, and §7 forbids a batch this scheduler would
//! happily run. The scheduler only bounds how many are outstanding, and counts the
//! high-water mark so §15 can report what was configured next to what was observed.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use futures_util::future::{join_all, BoxFuture};
use serde::Serialize;

use crate::state::ProviderResult;

/// One read a batch was asked to run.
///
/// The four fields are the four the RPC trace already publishes for the call this
/// read becomes — `chain_id`, `block`, `target`, and the method [`kind`] names —
/// chosen so a reader can join a batch to the calls it produced without this type
/// restating `evm-chain`'s dedup key, which is that crate's to define and would
/// drift the moment it were copied.
///
/// [`kind`]: StateReadDescriptor::kind
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StateReadDescriptor {
    /// `balance`, `nonce` or `code`: the three §5's dependency analysis found to be
    /// independent of one another. Storage is deliberately not in that list — a
    /// slot is demanded by the EVM one at a time, so it is never a batch member
    /// (§7), and a scheduler that was handed one would be an invitation.
    pub kind: &'static str,
    pub chain_id: u64,
    pub block: u64,
    /// Lowercase, because the trace's `target` is lowercase and a join that is
    /// case-sensitive on an address is a join that fails for a reason that has
    /// nothing to do with the state.
    pub address: String,
}

/// A hand of reads this scheduler was given, as it was given them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BatchDispatch {
    /// Which read site opened the batch: `account_triple` or `touched_contracts`.
    pub site: &'static str,
    /// The bound in force when the batch ran — the configured value, since a chunk
    /// can be shorter than the bound at the tail of a batch, never longer.
    pub limit: usize,
    /// The reads in input order. A member answered from this simulation's own cache
    /// is still listed: the batch is the *plan*, and the trace beside it says which
    /// members cost a request.
    pub reads: Vec<StateReadDescriptor>,
}

/// What one simulation's scheduler can say afterwards, for the evidence (§15).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ConcurrencyReport {
    /// The bound the run was configured with, after `0` has been read as `1`.
    pub configured: usize,
    /// `configured == 1`: the arm that has to reproduce M8.3.2's baseline (§10).
    pub serial: bool,
    /// Batches this scheduler ran.
    pub batches: usize,
    /// Reads handed to those batches, counted per member.
    pub batched_reads: usize,
    /// Most reads this provider had outstanding at the node at one instant,
    /// counted by the scheduler's own bracket around each dispatch — not the bound
    /// it was given. §15: a run that configured 4 and peaked at 1 is a run that
    /// never ran 4, and only this number can say so.
    pub observed_peak: usize,
    /// The batches themselves, in the order they were opened.
    pub batch_dispatches: Vec<BatchDispatch>,
    /// What a reader must not infer from the two numbers above, in the evidence's
    /// own words: `observed_peak` is concurrency at this provider's boundary, while
    /// the trace's `max_concurrency` is concurrency of the calls on the wire. The
    /// two are only equal when every outstanding read became a request, and a cache
    /// hit is why they sometimes are not.
    pub note: &'static str,
}

/// `ConcurrencyReport::note`, verbatim.
///
/// The three clauses are each a fact a reader could otherwise get backwards: a cache hit
/// is not a concurrency, a retry is not a second read, and a bound is not a measurement.
pub const CONCURRENCY_NOTE: &str = "observed_peak counts the state reads this simulation's \
    provider held outstanding at the node at one instant; a read answered from this \
    simulation's own cache never enters it, and a read the adapter retried is still one read \
    here while the wire trace counts its attempts. configured is the bound that was asked \
    for and is never evidence that it happened (§15).";

/// One simulation's bounded dispatch.
///
/// Owned by one provider and shared with the providers `with_setup` derives from it,
/// so the cache's lifetime rule (M8.3.1 §2: one simulation, one cache) is this type's
/// lifetime rule too — there is no second scheduler for a later run to find.
#[derive(Clone, Debug)]
pub struct BoundedDispatch {
    limit: usize,
    in_flight: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
    batches: Arc<Mutex<Vec<BatchDispatch>>>,
}

/// One read's bracket around its dispatch.
///
/// A guard rather than an explicit leave call because a future dropped partway —
/// the whole simulation cancelled, not a failed sibling — would otherwise strand
/// the count high, and a high-water mark that can only be too large is a number an
/// evidence file should not carry.
pub(crate) struct InFlight(Arc<AtomicUsize>);

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl BoundedDispatch {
    /// The scheduler for one simulation. `0` is read as `1`: a bound of zero would
    /// mean a batch that can run nothing, which is a refusal of the run rather than
    /// a concurrency setting, and §27 wants an odd input kept out of the experiment
    /// rather than allowed to stall it.
    pub fn new(limit: usize) -> Self {
        Self {
            limit: limit.max(1),
            in_flight: Arc::new(AtomicUsize::new(0)),
            peak: Arc::new(AtomicUsize::new(0)),
            batches: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// The bound, as the run asked for it.
    pub const fn configured(&self) -> usize {
        self.limit
    }

    /// Whether this run is the serial one. Read off the bound, not the peak: §10's
    /// baseline arm is the one that was *told* to run one at a time.
    pub const fn is_serial(&self) -> bool {
        self.limit == 1
    }

    pub fn observed_peak(&self) -> usize {
        self.peak.load(Ordering::Acquire)
    }

    /// The run's own account of its dispatch, for the trace line.
    pub fn report(&self) -> ConcurrencyReport {
        let batches = self.batch_log().clone();
        let batched_reads = batches.iter().map(|batch| batch.reads.len()).sum();
        ConcurrencyReport {
            configured: self.limit,
            serial: self.is_serial(),
            batches: batches.len(),
            batched_reads,
            observed_peak: self.observed_peak(),
            batch_dispatches: batches,
            note: CONCURRENCY_NOTE,
        }
    }

    /// Bracket one read's request while it is outstanding at the node.
    ///
    /// Called by the provider around the `ChainAdapter` await, not around the whole
    /// task: a read answered from this simulation's cache never goes out, and
    /// counting it would put an overlap in the evidence that the wire did not have
    /// (§16's rule that an instrument must not become a second measurement).
    pub(crate) fn enter(&self) -> InFlight {
        let outstanding = self.in_flight.fetch_add(1, Ordering::AcqRel) + 1;
        self.peak.fetch_max(outstanding, Ordering::AcqRel);
        InFlight(Arc::clone(&self.in_flight))
    }

    /// Run `tasks` with no more than `limit` of them awaited at a time, and return
    /// their results in input order.
    ///
    /// Chunks are taken from the front, so a batch of three under a bound of two is
    /// `[0, 1]` then `[2]` — a bounded run, not a sliding window. That is the whole
    /// of the scheduler §9 asks for: the reads are already decided by the caller,
    /// and nothing here creates a task the caller did not hand it.
    ///
    /// A chunk that carries a failure ends the batch: no later chunk starts, so the
    /// serial arm stops at exactly the read `?` would have stopped at (M8.3.3 §10),
    /// and a concurrent arm asks the node for at most one chunk's worth of reads the
    /// serial arm would not have made. Those extra reads are a fact about a failing
    /// batch, reported by the caller rather than undone by dropping the futures of
    /// requests that already left — a dropped future leaves the wire with a call no
    /// trace event explains, which is the one thing §16 forbids.
    pub(crate) async fn run<'a, T: 'a>(
        &self,
        site: &'static str,
        reads: Vec<StateReadDescriptor>,
        tasks: Vec<BoxFuture<'a, ProviderResult<T>>>,
    ) -> Vec<ProviderResult<T>> {
        let count = tasks.len();
        self.batch_log().push(BatchDispatch {
            site,
            limit: self.limit,
            reads,
        });
        let mut queue: VecDeque<_> = tasks.into_iter().collect();
        let mut done = Vec::with_capacity(count);
        while !queue.is_empty() {
            let take = self.limit.min(queue.len());
            let chunk: Vec<_> = queue.drain(..take).collect();
            let results = join_all(chunk).await;
            let failed = results.iter().any(|result| result.is_err());
            done.extend(results);
            if failed {
                break;
            }
        }
        done
    }

    /// Poison-resistant, like the reuse cache beside it: a panic elsewhere in the
    /// process must not turn a state read into a panic here, and a log that lost a
    /// batch would report fewer reads than the run made. Held only long enough to
    /// push — no lock is taken across an await.
    fn batch_log(&self) -> MutexGuard<'_, Vec<BatchDispatch>> {
        match self.batches.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
    use std::time::Duration;

    use futures_util::future::BoxFuture;

    use super::{BoundedDispatch, StateReadDescriptor, CONCURRENCY_NOTE};
    use crate::state::{ProviderError, ProviderResult};

    /// Every scheduled task parks on this before answering. One millisecond is long
    /// enough that a chunk's members are provably outstanding together — on the
    /// current-thread runtime a task only reaches its timer once, and the next chunk is
    /// not polled until `join_all` has finished with this one — and short enough that
    /// the whole matrix costs the suite nothing.
    const HOLD: Duration = Duration::from_millis(1);

    /// A wait that outlasts [`HOLD`] by a wide margin, for the one test that needs the
    /// members of a chunk to answer in an order other than the order they were handed
    /// over in.
    const LATE: Duration = Duration::from_millis(20);

    /// The pin the live experiment uses, so a descriptor here is the same shape the
    /// evidence will carry rather than a stand-in with different fields.
    const PIN: u64 = 37_191_169;

    fn descriptor(index: usize) -> StateReadDescriptor {
        StateReadDescriptor {
            kind: "balance",
            chain_id: 91342,
            block: PIN,
            address: format!("0x{index:040x}"),
        }
    }

    /// `count` reads starting at `first`, in input order. The addresses come from the
    /// same sequence the task indices use, so a hand and its tasks name the same reads.
    fn hand_from(first: usize, count: usize) -> Vec<StateReadDescriptor> {
        (first..first + count).map(descriptor).collect()
    }

    fn hand(count: usize) -> Vec<StateReadDescriptor> {
        hand_from(0, count)
    }

    /// What the tasks wrote, in the order they wrote it. A shared cell rather than a
    /// returned `Vec` because a task cannot hand anything back before its chunk has
    /// finished, and everything under test here happens *inside* a chunk.
    #[derive(Clone, Default)]
    struct Watch(Arc<Mutex<Vec<String>>>);

    impl Watch {
        fn record(&self, event: &str) {
            self.lock().push(event.to_string());
        }

        fn events(&self) -> Vec<String> {
            self.lock().clone()
        }

        fn lock(&self) -> MutexGuard<'_, Vec<String>> {
            self.0.lock().unwrap_or_else(PoisonError::into_inner)
        }
    }

    /// One read of the batch: it logs its start, brackets the dispatch the way the
    /// provider brackets its wire await, waits, logs its end, and answers with its own
    /// index so the caller can see whether results kept their order.
    ///
    /// `bracketed` is the difference between a read that costs the node a request and
    /// one this simulation answered from its own cache: the second never goes out, and
    /// §16 forbids an instrument from counting it. The wait is identical either way, so
    /// a reported peak of zero is a statement about the bracket and not about the
    /// schedule.
    fn read(
        dispatch: &Arc<BoundedDispatch>,
        watch: &Watch,
        index: usize,
        hold: Duration,
        failures: &[usize],
        bracketed: bool,
    ) -> BoxFuture<'static, ProviderResult<usize>> {
        let dispatch = Arc::clone(dispatch);
        let watch = watch.clone();
        let failed = failures.contains(&index);
        Box::pin(async move {
            watch.record(&format!("start-{index}"));
            let outstanding = bracketed.then(|| dispatch.enter());
            if !hold.is_zero() {
                tokio::time::sleep(hold).await;
            }
            watch.record(&format!("end-{index}"));
            drop(outstanding);
            if failed {
                Err(ProviderError::Unavailable {
                    provider: "test".to_string(),
                    reason: format!("read {index} did not answer"),
                })
            } else {
                Ok(index)
            }
        })
    }

    /// A read that went to the node.
    fn wire(
        dispatch: &Arc<BoundedDispatch>,
        watch: &Watch,
        index: usize,
        hold: Duration,
        failures: &[usize],
    ) -> BoxFuture<'static, ProviderResult<usize>> {
        read(dispatch, watch, index, hold, failures, true)
    }

    /// A read answered from this simulation's own cache.
    fn cached(
        dispatch: &Arc<BoundedDispatch>,
        watch: &Watch,
        index: usize,
    ) -> BoxFuture<'static, ProviderResult<usize>> {
        read(dispatch, watch, index, HOLD, &[], false)
    }

    /// `count` tasks at the given bound, all waiting `HOLD`, none failing.
    fn all_wire(
        dispatch: &Arc<BoundedDispatch>,
        watch: &Watch,
        count: usize,
    ) -> Vec<BoxFuture<'static, ProviderResult<usize>>> {
        (0..count)
            .map(|index| wire(dispatch, watch, index, HOLD, &[]))
            .collect()
    }

    /// The index of an event in the log. Kept as `Option` and turned into a message by
    /// the caller: a missing `start-5` is the result a test about a truncated batch is
    /// looking for, and a bare `unwrap` would report a panic instead of it.
    fn position(events: &[String], event: &str) -> usize {
        events
            .iter()
            .position(|written| written == event)
            .unwrap_or_else(|| panic!("`{event}` was never logged; the run wrote {events:?}"))
    }

    /// §37's first row, and §10's mechanism: at the bound of one this scheduler is the
    /// sequential code it replaced — one read outstanding, in input order, and the next
    /// one not even started until the previous one has answered.
    #[tokio::test]
    async fn concurrency_one_runs_its_reads_one_at_a_time_in_order() {
        let dispatch = Arc::new(BoundedDispatch::new(1));
        let watch = Watch::default();
        let results = dispatch
            .run("account_triple", hand(5), all_wire(&dispatch, &watch, 5))
            .await;

        assert_eq!(
            watch.events(),
            (0..5)
                .flat_map(|index| [format!("start-{index}"), format!("end-{index}")])
                .collect::<Vec<_>>(),
            "serial means nothing overlaps, so the log is fully determined"
        );
        assert_eq!(
            results.into_iter().map(Result::unwrap).collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4],
            "results come back in input order"
        );
        assert_eq!(dispatch.observed_peak(), 1, "one at a time");
        let report = dispatch.report();
        assert!(report.serial);
        assert_eq!(report.configured, 1);
        assert_eq!(
            report.batches, 1,
            "one hand, however many chunks it cut it into"
        );
        assert_eq!(report.batched_reads, 5);
    }

    /// §37's second row: two reads are outstanding together, and the third does not
    /// join them — the bound is a wall, not a sliding window that refills as soon as a
    /// slot opens.
    #[tokio::test]
    async fn concurrency_two_overlaps_a_pair_then_waits_for_both() {
        let dispatch = Arc::new(BoundedDispatch::new(2));
        let watch = Watch::default();
        let results = dispatch
            .run("touched_contracts", hand(5), all_wire(&dispatch, &watch, 5))
            .await;

        let events = watch.events();
        // Both members of the first chunk are polled before either can answer, because
        // `join_all` drives its whole set. That is the overlap §12 wants proven.
        assert!(
            position(&events, "start-0") < position(&events, "end-0")
                && position(&events, "start-1") < position(&events, "end-0"),
            "read 1 was not outstanding while read 0 waited: {events:?}"
        );
        // And the next chunk waits for the *pair*, not for the first one to free a slot.
        for finished in ["end-0", "end-1"] {
            assert!(
                position(&events, finished) < position(&events, "start-2"),
                "`{finished}` happened after read 2 had already started, so this was a \
                 sliding window and not the bound §9 asked for: {events:?}"
            );
        }
        assert_eq!(
            results.into_iter().map(Result::unwrap).collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4],
            "two chunks of a batch finishing in timer order still answer in input order"
        );
        assert_eq!(dispatch.observed_peak(), 2);
        assert!(!dispatch.report().serial);
    }

    /// §37's third row, with the tail: seven reads at the bound of four is a chunk of
    /// four and a chunk of three. The short last chunk must not be reported as if four
    /// had been outstanding, and the whole batch is still one entry in the log.
    #[tokio::test]
    async fn concurrency_four_runs_four_at_a_time_and_the_tail_at_three() {
        let dispatch = Arc::new(BoundedDispatch::new(4));
        let watch = Watch::default();
        let results = dispatch
            .run("touched_contracts", hand(7), all_wire(&dispatch, &watch, 7))
            .await;

        let events = watch.events();
        assert_eq!(
            results.into_iter().map(Result::unwrap).collect::<Vec<_>>(),
            (0..7).collect::<Vec<_>>()
        );
        assert_eq!(dispatch.observed_peak(), 4, "four reads of the first chunk");
        // The tail is genuinely shorter: with only three reads left the run cannot reach
        // the bound, and `observed_peak` reports the highest instant it did reach.
        for pending in 4..7 {
            assert!(
                position(&events, &format!("start-{pending}")) > position(&events, "end-3"),
                "the tail started before the first chunk had finished"
            );
        }
        let report = dispatch.report();
        assert_eq!(report.batches, 1);
        assert_eq!(report.batched_reads, 7);
        assert_eq!(report.batch_dispatches[0].limit, 4);
    }

    /// §15, read from the scheduler's own two numbers: a bound is a ceiling, not a
    /// target. Two reads at the bound of four overlap by two and report four as the
    /// configuration — and a reader who conflated the two would credit this run with a
    /// concurrency it never had.
    #[tokio::test]
    async fn a_batch_shorter_than_the_bound_reports_the_peak_it_reached() {
        let dispatch = Arc::new(BoundedDispatch::new(4));
        let watch = Watch::default();
        dispatch
            .run("touched_contracts", hand(2), all_wire(&dispatch, &watch, 2))
            .await;

        let report = dispatch.report();
        assert_eq!(report.configured, 4, "what the run asked for");
        assert_eq!(
            report.observed_peak, 2,
            "what the node was actually holding"
        );
        assert!(
            report.observed_peak < report.configured,
            "the report has to be able to say so out loud, not only in the reader's head"
        );
        assert!(
            !report.serial,
            "this is not the baseline arm even though it never held more than two reads \
             — `serial` is read off the bound, and §10's comparison depends on that"
        );
    }

    /// The widest instant the log itself describes, counted from the tasks' own events
    /// rather than from the scheduler's instrument.
    fn peak_in_log(events: &[String]) -> usize {
        let mut outstanding = 0usize;
        let mut widest = 0usize;
        for event in events {
            if event.starts_with("start-") {
                outstanding += 1;
                widest = widest.max(outstanding);
            } else {
                outstanding = outstanding.checked_sub(1).unwrap_or_else(|| {
                    panic!("an end was logged with nothing outstanding: {events:?}")
                });
            }
        }
        widest
    }

    /// §37's "maximum concurrency enforcement", as a sweep rather than one spot check:
    /// with more reads than the bound, a run reaches its bound and never passes it.
    /// Eight is the largest bound the command line accepts (§27), so the sweep covers
    /// every value the experiment can be configured with.
    ///
    /// Each row also recounts the widest instant from the tasks' own start/end events,
    /// independently of the scheduler's instrument, and the two have to agree —
    /// otherwise `observed_peak` would be a number about the instrument rather than
    /// about the run (§16).
    #[tokio::test]
    async fn no_bound_is_ever_exceeded() {
        for limit in 1..=8usize {
            let dispatch = Arc::new(BoundedDispatch::new(limit));
            let watch = Watch::default();
            let reads = hand(limit + 1);
            dispatch
                .run(
                    "account_triple",
                    reads.clone(),
                    all_wire(&dispatch, &watch, limit + 1),
                )
                .await;

            let events = watch.events();
            assert_eq!(
                dispatch.observed_peak(),
                limit,
                "{limit} reads were available to overlap at the bound of {limit}, so the \
                 peak should have reached it"
            );
            assert_eq!(
                peak_in_log(&events),
                limit,
                "the instrument and the task log disagree about how many reads were \
                 outstanding at once: {events:?}"
            );
            assert_eq!(dispatch.report().batch_dispatches[0].reads, reads);
        }
    }

    /// §27's odd inputs do not reach this type at all — the command line refuses `0`,
    /// a negative and an overflow before a simulation starts, which `crates/cli` tests.
    /// What is tested here is the second half of that rule: if a `0` ever did arrive, it
    /// means serial rather than a batch that can run nothing. A bound of zero would
    /// stall a state read, and a stall is not a concurrency setting.
    #[tokio::test]
    async fn a_bound_of_zero_is_serial_not_stalled() {
        let dispatch = Arc::new(BoundedDispatch::new(0));
        assert_eq!(dispatch.configured(), 1);
        assert!(dispatch.is_serial());

        let watch = Watch::default();
        let results = dispatch
            .run("account_triple", hand(3), all_wire(&dispatch, &watch, 3))
            .await;
        assert_eq!(results.len(), 3, "the batch ran instead of hanging");
        assert_eq!(
            watch.events(),
            vec!["start-0", "end-0", "start-1", "end-1", "start-2", "end-2"]
        );
        assert_eq!(dispatch.observed_peak(), 1);
    }

    /// §17's premise, in one place: the order the reads finish in is not the order the
    /// caller sees them in. Read 0 answers last and still comes back first, so the
    /// dump's markers and the folded account state cannot depend on which request the
    /// node answered sooner — a fixture's bytes would otherwise differ between two runs
    /// of the same arm.
    #[tokio::test]
    async fn results_keep_input_order_when_reads_finish_in_another_order() {
        let dispatch = Arc::new(BoundedDispatch::new(2));
        let watch = Watch::default();
        let tasks = vec![
            wire(&dispatch, &watch, 0, LATE, &[]),
            wire(&dispatch, &watch, 1, HOLD, &[]),
        ];
        let results = dispatch.run("touched_contracts", hand(2), tasks).await;

        let events = watch.events();
        assert!(
            position(&events, "end-1") < position(&events, "end-0"),
            "this test is only testing anything if the second read answered first: {events:?}"
        );
        assert_eq!(
            results.into_iter().map(Result::unwrap).collect::<Vec<_>>(),
            vec![0, 1]
        );
    }

    /// §16, the half that a cache makes invisible: four reads overlapping in every
    /// other way report a peak of zero, because none of them bracketed a dispatch. A
    /// reused state read is not an overlap, and an evidence file that said otherwise
    /// would be measuring the scheduler instead of the wire.
    #[tokio::test]
    async fn a_read_answered_from_cache_adds_no_concurrency() {
        let dispatch = Arc::new(BoundedDispatch::new(4));
        let watch = Watch::default();
        let tasks = (0..4)
            .map(|index| cached(&dispatch, &watch, index))
            .collect();
        let results = dispatch.run("touched_contracts", hand(4), tasks).await;

        assert_eq!(results.len(), 4, "the cache still answers");
        assert_eq!(dispatch.observed_peak(), 0);
        assert_eq!(
            dispatch.report().batched_reads,
            4,
            "the batch is the plan the caller handed over; the peak beside it is what \
             actually went out, and the two are different numbers on purpose"
        );

        // And one bracket on its own is not a high-water mark: three reads in a row peak
        // at one, because `peak` is the widest instant, not a count of reads.
        let serial = BoundedDispatch::new(1);
        for _ in 0..3 {
            drop(serial.enter());
        }
        assert_eq!(serial.observed_peak(), 1, "three reads, never two at once");
    }

    /// §9's failure rule, and the reason the scheduler is a chunked `join_all` rather
    /// than `try_join_all`: when read 2 of a chunk fails, its three siblings have
    /// already left for the node. Their futures must be awaited or the trace would carry
    /// calls with no events behind them, so the chunk finishes and the batch ends there.
    /// Reads 4–6 are never asked for.
    #[tokio::test]
    async fn a_failing_read_finishes_its_chunk_and_ends_the_batch() {
        let dispatch = Arc::new(BoundedDispatch::new(4));
        let watch = Watch::default();
        let tasks = (0..7)
            .map(|index| wire(&dispatch, &watch, index, HOLD, &[2]))
            .collect();
        let results = dispatch.run("touched_contracts", hand(7), tasks).await;

        let events = watch.events();
        assert_eq!(results.len(), 4, "one chunk, then the batch stopped");
        assert!(
            results[2].is_err(),
            "the failure stays at its input position"
        );
        assert!(results[0].is_ok() && results[1].is_ok() && results[3].is_ok());
        for index in 4..7 {
            assert!(
                !events.contains(&format!("start-{index}")),
                "the batch started a chunk after the failure, so the caller's `?` would \
                 have stopped earlier than this run did: {events:?}"
            );
        }
        for index in 0..4 {
            assert!(
                events.contains(&format!("end-{index}")),
                "read {index} was sent and then dropped without being awaited, which \
                 leaves a call on the wire that no trace event explains (§16)"
            );
        }
        assert_eq!(
            dispatch.report().batched_reads,
            7,
            "the hand was seven reads even though four cost a request"
        );
    }

    /// §10's baseline, on the failing path too: at the bound of one a failure stops at
    /// exactly the read the sequential code stopped at, so C1 and M8.3.2's runs differ
    /// by nothing here either. Two of the three reads never go out, as they never would
    /// have.
    #[tokio::test]
    async fn serial_stops_at_the_read_that_failed() {
        let dispatch = Arc::new(BoundedDispatch::new(1));
        let watch = Watch::default();
        let tasks = (0..4)
            .map(|index| wire(&dispatch, &watch, index, Duration::ZERO, &[2]))
            .collect();
        let results = dispatch.run("account_triple", hand(4), tasks).await;

        assert_eq!(results.len(), 3, "reads 0 and 1 answered, read 2 failed");
        assert_eq!(
            watch.events(),
            vec!["start-0", "end-0", "start-1", "end-1", "start-2", "end-2"]
        );
        assert!(results[2].is_err());
        assert!(!watch.events().contains(&"start-3".to_string()));
    }

    /// One scheduler per simulation, shared by every provider derived from it
    /// (`with_setup` clones the `Arc`): a derived provider's reads land in the same
    /// peak and the same batch log, so there is no second scheduler for a later run to
    /// find and no arm whose concurrency was measured against a different instrument.
    #[tokio::test]
    async fn a_derived_provider_shares_this_simulations_scheduler() {
        let dispatch = Arc::new(BoundedDispatch::new(2));
        let derived = Arc::clone(&dispatch);
        let watch = Watch::default();
        derived
            .run("touched_contracts", hand(2), all_wire(&derived, &watch, 2))
            .await;

        assert_eq!(
            dispatch.observed_peak(),
            2,
            "the original saw the derived provider's overlap"
        );
        let report = dispatch.report();
        assert_eq!(report.batches, 1);
        assert_eq!(
            dispatch.report(),
            report,
            "and both halves of the pair report one run's account"
        );
    }

    /// §15's report is the evidence line this milestone publishes, so its parts are
    /// checked here rather than inferred from a live run: the bound, the serial flag,
    /// one entry per hand in the order they were opened, the reads of each hand exactly
    /// as given, and the note that says what the two numbers must not be read as.
    #[tokio::test]
    async fn the_report_is_the_runs_own_account() {
        let arc = Arc::new(BoundedDispatch::new(3));
        let watch = Watch::default();

        // Two hands, opened in order, at a bound of three.
        let first = hand(4);
        let second = hand_from(4, 2);
        let tasks = (0..4)
            .map(|index| wire(&arc, &watch, index, HOLD, &[]))
            .collect();
        arc.run("account_triple", first.clone(), tasks).await;
        let tasks = (4..6)
            .map(|index| wire(&arc, &watch, index, HOLD, &[]))
            .collect();
        arc.run("touched_contracts", second.clone(), tasks).await;

        let report = arc.report();
        assert_eq!(report.configured, 3);
        assert!(!report.serial);
        assert_eq!(report.batches, 2);
        assert_eq!(report.batched_reads, 6, "four reads plus two");
        assert_eq!(report.batch_dispatches[0].site, "account_triple");
        assert_eq!(report.batch_dispatches[1].site, "touched_contracts");
        assert_eq!(report.batch_dispatches[0].reads, first);
        assert_eq!(report.batch_dispatches[1].reads, second);
        assert_eq!(report.batch_dispatches[0].limit, 3);
        assert_eq!(
            report.observed_peak, 3,
            "the first hand of four held three of them at once, and the hand of two after \
             it could not raise the mark"
        );
        assert_eq!(
            report.note, CONCURRENCY_NOTE,
            "the note is compared verbatim by the diagnosis line, so it is part of the \
             schema here and not free text"
        );
    }

    /// §16 again, from the scheduler's side: it bounds dispatch, it does not decide
    /// what gets dispatched. A hand of three reads costs three calls at any bound, and
    /// a bound of four on a hand of three asks for nothing the caller did not name — no
    /// prefetch, no speculative fourth read, no batch request that would have merged
    /// the three (§3).
    #[tokio::test]
    async fn the_scheduler_creates_no_read_the_caller_did_not_ask_for() {
        for limit in [1usize, 2, 4, 8] {
            let dispatch = Arc::new(BoundedDispatch::new(limit));
            let watch = Watch::default();
            let results = dispatch
                .run("touched_contracts", hand(3), all_wire(&dispatch, &watch, 3))
                .await;

            let events = watch.events();
            assert_eq!(results.len(), 3);
            assert_eq!(
                events
                    .iter()
                    .filter(|event| event.starts_with("start-"))
                    .count(),
                3,
                "bound {limit} ran more or fewer than the three reads it was handed"
            );
            assert_eq!(
                dispatch.report().batched_reads,
                3,
                "and the log claims exactly them"
            );
        }
    }
}
