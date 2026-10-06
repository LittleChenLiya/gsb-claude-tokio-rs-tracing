//! Tests for the `RecordPlan` span field recording plan.
//!
//! These tests verify that a recording plan associates initialized fields
//! and subsequent `record`s by their actual `Field` keys and callsite
//! identity, so that same-named fields declared by two different callsites
//! remain independent, and that generating a plan never inspects the
//! recorded values.
#![cfg(feature = "alloc")]

use std::fmt;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

use tracing_core::{
    callsite::Callsite,
    field::Value,
    metadata::Kind,
    span::{Attributes, Id, Record},
    Event, Metadata, Subscriber,
};
use tracing_subscriber::field::{FieldState, RecordPlan};

/// Two distinct callsites (`span_a` and `span_b`) that both declare a field
/// named `field`.
fn two_callsites() -> (&'static Metadata<'static>, &'static Metadata<'static>) {
    let callsite_a = tracing::callsite!(name: "span_a", kind: Kind::SPAN, fields: field);
    let callsite_b = tracing::callsite!(name: "span_b", kind: Kind::SPAN, fields: field);
    (callsite_a.metadata(), callsite_b.metadata())
}

#[test]
fn same_named_fields_from_two_callsites_stay_independent() {
    let (meta_a, meta_b) = two_callsites();
    let field_a = meta_a.fields().field("field").unwrap();
    let field_b = meta_b.fields().field("field").unwrap();

    // The fields have the same name, but distinct identities.
    assert_eq!(field_a.name(), field_b.name());
    assert_ne!(field_a, field_b);
    assert_ne!(field_a.callsite(), field_b.callsite());

    // `span_a` is initialized with a value; `span_b` is not.
    let values_a = [(&field_a, Some(&1_u64 as &dyn Value))];
    let values_a = meta_a.fields().value_set(&values_a);
    let attrs_a = Attributes::new(meta_a, &values_a);
    let mut plan_a = RecordPlan::new(&attrs_a);
    let mut plan_b = RecordPlan::from_metadata(meta_b);

    assert_eq!(plan_a.state(&field_a), Some(FieldState::Initialized));
    assert_eq!(plan_b.state(&field_b), Some(FieldState::Pending));

    // Looking up the *other* callsite's same-named field in a plan does not
    // find anything: the two fields are not merged by name.
    assert_eq!(plan_a.state(&field_b), None);
    assert_eq!(plan_b.state(&field_a), None);
    assert!(!plan_a.contains(&field_b));
    assert!(!plan_b.contains(&field_a));

    // A `Record` belonging to `span_b`'s callsite must not be associated
    // with `span_a`'s plan, even though the field names match.
    let values_b = [(&field_b, Some(&2_u64 as &dyn Value))];
    let values_b = meta_b.fields().value_set(&values_b);
    let record_b = Record::new(&values_b);

    plan_a.record(&record_b);
    assert_eq!(
        plan_a.state(&field_a),
        Some(FieldState::Initialized),
        "a record from another callsite must not change this plan",
    );
    assert!(plan_a.recorded().next().is_none());

    plan_b.record(&record_b);
    assert_eq!(plan_b.state(&field_b), Some(FieldState::Recorded));
    assert_eq!(plan_b.recorded().collect::<Vec<_>>(), vec![field_b]);
}

#[test]
fn record_is_associated_with_the_original_field() {
    // A span whose `parting` field is declared but left empty at creation.
    let callsite = tracing::callsite! {
        name: "my_span",
        kind: Kind::SPAN,
        fields: greeting, parting
    };
    let meta = callsite.metadata();
    let greeting = meta.fields().field("greeting").unwrap();
    let parting = meta.fields().field("parting").unwrap();

    let values = [(&greeting, Some(&"hello" as &dyn Value))];
    let values = meta.fields().value_set(&values);
    let attrs = Attributes::new(meta, &values);

    let mut plan = RecordPlan::new(&attrs);
    assert_eq!(plan.len(), 2);
    assert!(!plan.is_empty());
    assert_eq!(plan.state(&greeting), Some(FieldState::Initialized));
    assert_eq!(plan.state(&parting), Some(FieldState::Pending));
    assert_eq!(plan.pending().collect::<Vec<_>>(), vec![parting.clone()]);
    assert_eq!(
        plan.initialized().collect::<Vec<_>>(),
        vec![greeting.clone()]
    );
    assert!(plan.recorded().next().is_none());

    // Recording a value later associates it with the *original* `Field`
    // declared by the span's metadata.
    let record_values = [(&parting, Some(&"goodbye" as &dyn Value))];
    let record_values = meta.fields().value_set(&record_values);
    plan.record(&Record::new(&record_values));

    assert_eq!(plan.state(&parting), Some(FieldState::Recorded));
    assert_eq!(plan.state(&greeting), Some(FieldState::Initialized));
    assert!(plan.pending().next().is_none());
    assert_eq!(plan.recorded().collect::<Vec<_>>(), vec![parting.clone()]);

    // The plan's report keeps only the source of each field: its name, its
    // callsite, and its recording state. No values appear in it.
    let report = format!("{:?}", plan);
    assert!(report.contains("my_span"), "report: {}", report);
    assert!(report.contains("greeting"), "report: {}", report);
    assert!(report.contains("parting"), "report: {}", report);
    assert!(!report.contains("hello"), "report: {}", report);
    assert!(!report.contains("goodbye"), "report: {}", report);
}

/// A value whose `fmt::Debug` implementation counts how often it is called.
struct CountsDebug<'a>(&'a AtomicUsize);

impl fmt::Debug for CountsDebug<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fetch_add(1, Ordering::Relaxed);
        f.write_str("CountsDebug")
    }
}

#[test]
fn generating_a_plan_does_not_inspect_values() {
    let callsite = tracing::callsite!(name: "counted", kind: Kind::SPAN, fields: field);
    let meta = callsite.metadata();
    let field = meta.fields().field("field").unwrap();

    let count = AtomicUsize::new(0);
    let value = tracing_core::field::debug(CountsDebug(&count));

    let values = [(&field, Some(&value as &dyn Value))];
    let values = meta.fields().value_set(&values);
    let attrs = Attributes::new(meta, &values);

    // Generating a plan from the attributes...
    let mut plan = RecordPlan::new(&attrs);
    assert_eq!(plan.state(&field), Some(FieldState::Initialized));

    // ...and updating it with a record must not format the value.
    plan.record(&Record::new(&values));

    assert_eq!(
        count.load(Ordering::Relaxed),
        0,
        "generating the recording plan must not inspect field values",
    );

    // Generating the plan's report must not inspect values either.
    let _report = format!("{:?}", plan);
    assert_eq!(count.load(Ordering::Relaxed), 0);

    // Sanity check: the counter does work when the value is actually
    // formatted.
    let _ = format!("{:?}", CountsDebug(&count));
    assert_eq!(count.load(Ordering::Relaxed), 1);
}

/// A subscriber that generates a [`RecordPlan`] for every new span, updates
/// it on `record`, and counts the events it observes.
#[derive(Clone, Default)]
struct PlanCollector {
    plans: Arc<Mutex<Vec<(u64, RecordPlan)>>>,
    events: Arc<AtomicUsize>,
    next_id: Arc<AtomicUsize>,
}

impl PlanCollector {
    fn plan_for(&self, id: u64) -> RecordPlan {
        self.plans
            .lock()
            .unwrap()
            .iter()
            .find(|(span_id, _)| *span_id == id)
            .unwrap_or_else(|| panic!("no plan for span ID {}", id))
            .1
            .clone()
    }
}

impl Subscriber for PlanCollector {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, attrs: &Attributes<'_>) -> Id {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) as u64 + 1;
        self.plans
            .lock()
            .unwrap()
            .push((id, RecordPlan::new(attrs)));
        Id::from_u64(id)
    }

    fn record(&self, id: &Id, values: &Record<'_>) {
        let mut plans = self.plans.lock().unwrap();
        let (_, plan) = plans
            .iter_mut()
            .find(|(span_id, _)| *span_id == id.into_u64())
            .unwrap_or_else(|| panic!("no plan for span ID {:?}", id));
        plan.record(values);
    }

    fn event(&self, _: &Event<'_>) {
        self.events.fetch_add(1, Ordering::Relaxed);
    }

    fn record_follows_from(&self, _: &Id, _: &Id) {}

    fn enter(&self, _: &Id) {}

    fn exit(&self, _: &Id) {}
}

#[test]
fn record_plan_end_to_end_with_three_events() {
    let collector = PlanCollector::default();
    let handle = collector.clone();

    let (id_a, id_b, meta_a, meta_b) = tracing::subscriber::with_default(collector, || {
        // Two callsites, both declaring a field named `field`:
        let span_a = tracing::info_span!("span_a", field = tracing::field::Empty);
        let span_b = tracing::info_span!("span_b", field = tracing::field::Empty);

        // Record values later, using each span's actual `Field` key:
        let field_a = span_a.field("field").unwrap();
        span_a.record(&field_a, 1_u64);
        let field_b = span_b.field("field").unwrap();
        span_b.record(&field_b, 2_u64);

        // Three events:
        tracing::info!("event one");
        tracing::info!("event two");
        tracing::info!("event three");

        let id_a = span_a.id().unwrap().into_u64();
        let id_b = span_b.id().unwrap().into_u64();
        (
            id_a,
            id_b,
            span_a.metadata().unwrap(),
            span_b.metadata().unwrap(),
        )
    });

    // All three events were observed.
    assert_eq!(handle.events.load(Ordering::Relaxed), 3);

    let field_a = meta_a.fields().field("field").unwrap();
    let field_b = meta_b.fields().field("field").unwrap();
    assert_eq!(field_a.name(), field_b.name());

    let plan_a = handle.plan_for(id_a);
    let plan_b = handle.plan_for(id_b);

    // The two plans track two distinct callsites.
    assert_ne!(plan_a.callsite(), plan_b.callsite());

    // Each plan associated the subsequent `record` with the original `Field`
    // declared by its own callsite.
    assert_eq!(plan_a.state(&field_a), Some(FieldState::Recorded));
    assert_eq!(plan_b.state(&field_b), Some(FieldState::Recorded));

    // The two callsites' same-named fields remain independent.
    assert_eq!(plan_a.state(&field_b), None);
    assert_eq!(plan_b.state(&field_a), None);
}
