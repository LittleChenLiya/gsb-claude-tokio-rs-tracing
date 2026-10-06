//! Integration test for the span field recording plan (`RecordPlan`).
//!
//! Two span callsites declare a field with the same name (`shared`). The
//! recording plan keeps those fields independent, associates values recorded
//! later with the original `Field`, and never visits the recorded values
//! while the plan (or its sources report) is generated. Three events are
//! emitted to exercise the plan alongside ordinary event recording.
#![cfg(feature = "std")]

use std::{
    fmt,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use tracing::{
    field::{self, Value},
    info,
    subscriber::with_default,
    Level, Span,
};
use tracing_mock::*;

/// A value whose `fmt::Debug` implementation counts its invocations, so the
/// test can assert that generating a recording plan does not inspect the
/// recorded values.
struct CountingDebug {
    formats: Arc<AtomicUsize>,
}

impl CountingDebug {
    fn new() -> (Self, Arc<AtomicUsize>) {
        let formats = Arc::new(AtomicUsize::new(0));
        (
            CountingDebug {
                formats: formats.clone(),
            },
            formats,
        )
    }
}

impl fmt::Debug for CountingDebug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.formats.fetch_add(1, Ordering::SeqCst);
        f.write_str("CountingDebug")
    }
}

// Each `span!` invocation is a distinct callsite; both declare a field named
// `shared`.
fn span_a() -> Span {
    tracing::span!(
        Level::INFO,
        "plan_a",
        shared = field::Empty,
        only_a = field::Empty
    )
}

fn span_b() -> Span {
    tracing::span!(
        Level::INFO,
        "plan_b",
        shared = field::Empty,
        only_b = field::Empty
    )
}

#[test]
fn record_plan_keeps_same_named_fields_independent() {
    let (subscriber, handle) = subscriber::mock()
        .new_span(expect::span().named("plan_a"))
        .new_span(expect::span().named("plan_b"))
        .record(
            expect::span().named("plan_a"),
            expect::field("shared").with_value(&1_u64).only(),
        )
        .record(
            expect::span().named("plan_b"),
            expect::field("shared").with_value(&2_u64).only(),
        )
        // Three events are emitted while the spans exist.
        .event(expect::event().with_fields(expect::field("seq").with_value(&1_u64)))
        .event(expect::event().with_fields(expect::field("seq").with_value(&2_u64)))
        .event(expect::event().with_fields(expect::field("seq").with_value(&3_u64)))
        .drop_span(expect::span().named("plan_a"))
        .drop_span(expect::span().named("plan_b"))
        .only()
        .run_with_handle();

    with_default(subscriber, || {
        let span_a = span_a();
        let span_b = span_b();

        // Each span produces a recording plan tied to its own callsite.
        let plan_a = span_a.record_plan().expect("span_a is enabled");
        let plan_b = span_b.record_plan().expect("span_b is enabled");
        assert_ne!(plan_a.callsite(), plan_b.callsite());

        // Both callsites declare a field named `shared`; the plans resolve
        // the name to distinct `Field`s and never merge them.
        let shared_a = plan_a.field("shared").expect("plan_a declares `shared`");
        let shared_b = plan_b.field("shared").expect("plan_b declares `shared`");
        assert_eq!(shared_a.name(), shared_b.name());
        assert_ne!(shared_a, shared_b);
        assert!(plan_a.contains(&shared_a));
        assert!(!plan_a.contains(&shared_b));
        assert!(plan_b.contains(&shared_b));
        assert!(!plan_b.contains(&shared_a));

        // A value whose `fmt::Debug` counts invocations: generating the
        // plan, the sources report, and recording the values later must not
        // add any `Debug` calls beyond the ones performed by the subscriber
        // when it actually records the values.
        let (counting, formats) = CountingDebug::new();
        let counting = field::debug(counting);

        let meta_a = span_a.metadata().expect("span_a has metadata");
        let entries_a = [(&shared_a, Some(&counting as &dyn Value))];
        let values_a = meta_a.fields().value_set(&entries_a);

        // The sources report retains only the sources (callsite + `Field`).
        let sources = plan_a
            .sources(&values_a)
            .expect("plan_a accepts its own callsite's values");
        assert_eq!(sources.callsite(), plan_a.callsite());
        assert_eq!(sources.len(), 1);
        assert!(sources.contains(&shared_a));
        assert!(!sources.contains(&shared_b));
        assert_eq!(sources.fields().collect::<Vec<_>>(), vec![shared_a.clone()]);

        // A plan never accepts values from another callsite, even one that
        // declares a field with the same name.
        assert!(plan_b.sources(&values_a).is_none());
        assert!(plan_b.record(&values_a).is_none());

        // Generating the plan and the report did not visit the value.
        assert_eq!(
            formats.load(Ordering::SeqCst),
            0,
            "generating the recording plan must not format values"
        );

        // Record values later, through the plan, associated with the
        // original `Field` of each callsite.
        let one = 1_u64;
        let entries_a = [(&shared_a, Some(&one as &dyn Value))];
        let values_a = meta_a.fields().value_set(&entries_a);
        let record_a = plan_a
            .record(&values_a)
            .expect("plan_a records its own callsite's values");
        assert!(record_a.contains(&shared_a));
        assert!(!record_a.contains(&shared_b));
        span_a.record_all(&values_a);

        let meta_b = span_b.metadata().expect("span_b has metadata");
        let two = 2_u64;
        let entries_b = [(&shared_b, Some(&two as &dyn Value))];
        let values_b = meta_b.fields().value_set(&entries_b);
        assert!(plan_a.record(&values_b).is_none());
        span_b.record_all(&values_b);

        // Three events, emitted after the fields were recorded.
        info!(seq = 1_u64, "first event");
        info!(seq = 2_u64, "second event");
        info!(seq = 3_u64, "third event");

        drop(span_a);
        drop(span_b);
    });

    handle.assert_finished();
}
