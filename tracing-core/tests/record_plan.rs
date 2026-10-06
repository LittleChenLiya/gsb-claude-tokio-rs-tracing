//! Tests for the span field recording plan (`RecordPlan`) and its sources
//! report (`RecordSources`).
//!
//! These tests exercise the plan against two callsites that declare fields
//! with the *same* names, verifying that:
//!
//! - the two callsites' same-named fields are never merged by a plan,
//! - values recorded later are associated with the original `Field`,
//! - generating a plan (or a sources report) never visits the recorded
//!   values, so a `fmt::Debug` implementation that counts its invocations
//!   observes no additional calls.
#![cfg(feature = "std")]

use std::{
    fmt,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use tracing_core::{
    callsite::Callsite,
    field::{self, Field, Value, Visit},
    metadata,
    metadata::{Kind, Level, Metadata},
    span::{Record, RecordPlan},
    subscriber::Interest,
};

// The two test callsites are distinct types with non-zero size, so their
// `Identifier`s cannot coincide. Both declare a field named `shared`, plus
// one field unique to each callsite.
struct TestCallsiteA {
    _unused: u8,
}

static CALLSITE_A: TestCallsiteA = TestCallsiteA { _unused: 0 };

static META_A: Metadata<'static> = metadata! {
    name: "record_plan_test_a",
    target: module_path!(),
    level: Level::INFO,
    fields: &["shared", "only_a"],
    callsite: &CALLSITE_A,
    kind: Kind::SPAN,
};

impl Callsite for TestCallsiteA {
    fn set_interest(&self, _: Interest) {
        unimplemented!("test")
    }

    fn metadata(&self) -> &Metadata<'_> {
        &META_A
    }
}

struct TestCallsiteB {
    _unused: u8,
}

static CALLSITE_B: TestCallsiteB = TestCallsiteB { _unused: 0 };

static META_B: Metadata<'static> = metadata! {
    name: "record_plan_test_b",
    target: module_path!(),
    level: Level::INFO,
    fields: &["shared", "only_b"],
    callsite: &CALLSITE_B,
    kind: Kind::SPAN,
};

impl Callsite for TestCallsiteB {
    fn set_interest(&self, _: Interest) {
        unimplemented!("test")
    }

    fn metadata(&self) -> &Metadata<'_> {
        &META_B
    }
}

/// A value whose `fmt::Debug` implementation counts how many times it is
/// invoked, so tests can assert that generating a recording plan does not
/// inspect recorded values.
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

#[test]
fn same_named_fields_from_two_callsites_stay_independent() {
    let plan_a = RecordPlan::new(&META_A);
    let plan_b = RecordPlan::new(&META_B);

    // The plans are tied to distinct callsites.
    assert_ne!(plan_a.callsite(), plan_b.callsite());

    // Both callsites declare a field named `shared`, but each plan resolves
    // the name to its *own* callsite's `Field`.
    let shared_a = plan_a.field("shared").expect("callsite A declares `shared`");
    let shared_b = plan_b.field("shared").expect("callsite B declares `shared`");
    assert_eq!(shared_a.name(), shared_b.name());
    assert_ne!(shared_a, shared_b);
    assert_eq!(shared_a.callsite(), plan_a.callsite());
    assert_eq!(shared_b.callsite(), plan_b.callsite());

    // A plan only ever contains its own callsite's fields; the other
    // callsite's same-named field is not merged into it.
    assert!(plan_a.contains(&shared_a));
    assert!(!plan_a.contains(&shared_b));
    assert!(plan_b.contains(&shared_b));
    assert!(!plan_b.contains(&shared_a));

    // Fields that a callsite does not declare do not resolve, even if the
    // *other* callsite declares them.
    assert!(plan_a.field("only_a").is_some());
    assert!(plan_a.field("only_b").is_none());
    assert!(plan_b.field("only_b").is_some());
    assert!(plan_b.field("only_a").is_none());
}

#[test]
fn record_associates_values_with_the_original_field() {
    let plan_a = RecordPlan::new(&META_A);
    let plan_b = RecordPlan::new(&META_B);

    let shared_a = plan_a.field("shared").unwrap();
    let value = 42_u64;
    let entries = [(&shared_a, Some(&value as &dyn Value))];
    let values = META_A.fields().value_set(&entries);

    // The plan for callsite A records the values against the original
    // `Field` from callsite A...
    let record = plan_a
        .record(&values)
        .expect("plan A must accept values from callsite A");
    assert!(record.contains(&shared_a));
    assert_eq!(record.len(), 1);

    // ...while the plan for callsite B refuses to associate them with its
    // own same-named field.
    assert!(plan_b.record(&values).is_none());

    // The original `Record::new` entry point is unchanged and still
    // constructs a `Record` directly.
    let raw = Record::new(&values);
    assert_eq!(raw.len(), record.len());
    assert!(raw.contains(&shared_a));

    // The recorded values are visited with the original field.
    struct AssertField(Field);
    impl Visit for AssertField {
        fn record_debug(&mut self, _: &Field, _: &dyn fmt::Debug) {
            panic!("no debug values expected")
        }
        fn record_u64(&mut self, field: &Field, value: u64) {
            assert_eq!(*field, self.0);
            assert_eq!(value, 42);
        }
    }
    record.record(&mut AssertField(shared_a.clone()));
}

#[test]
fn sources_report_keeps_only_sources() {
    let plan_a = RecordPlan::new(&META_A);
    let plan_b = RecordPlan::new(&META_B);

    let (value, formats) = CountingDebug::new();
    let value = field::debug(value);

    let shared_a = plan_a.field("shared").unwrap();
    let only_a = plan_a.field("only_a").unwrap();
    let entries = [
        (&shared_a, Some(&value as &dyn Value)),
        (&only_a, Some(&value as &dyn Value)),
    ];
    let values = META_A.fields().value_set(&entries);

    // Generating the plan and the sources report must not visit the value...
    let sources = plan_a
        .sources(&values)
        .expect("plan A must accept values from callsite A");
    assert_eq!(sources.callsite(), plan_a.callsite());
    assert_eq!(sources.len(), 2);
    assert!(!sources.is_empty());
    assert!(sources.contains(&shared_a));
    assert!(sources.contains(&only_a));

    // ...and the report retains only the sources: the callsite and the
    // `Field`s, never the values.
    let reported: Vec<Field> = sources.fields().collect();
    assert_eq!(reported, vec![shared_a.clone(), only_a.clone()]);
    assert!(reported
        .iter()
        .all(|field| field.callsite() == plan_a.callsite()));

    // A plan for another callsite does not produce a report for these
    // values, even though it declares a field with the same name.
    assert!(plan_b.sources(&values).is_none());

    assert_eq!(
        formats.load(Ordering::SeqCst),
        0,
        "generating the plan and report must not format the recorded value"
    );

    // Actually recording the values *does* visit them, exactly once each.
    struct DebugVisitor;
    impl Visit for DebugVisitor {
        fn record_debug(&mut self, _: &Field, value: &dyn fmt::Debug) {
            let _ = format!("{:?}", value);
        }
    }
    let record = plan_a.record(&values).unwrap();
    record.record(&mut DebugVisitor);
    assert_eq!(
        formats.load(Ordering::SeqCst),
        2,
        "recording visits each value exactly once"
    );
}

#[test]
fn empty_value_set_produces_an_empty_report() {
    let plan_a = RecordPlan::new(&META_A);
    let values = META_A.fields().value_set(&[]);
    let sources = plan_a.sources(&values).unwrap();
    assert!(sources.is_empty());
    assert_eq!(sources.len(), 0);
    assert_eq!(sources.fields().count(), 0);
}
