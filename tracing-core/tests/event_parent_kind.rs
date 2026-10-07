use tracing_core::{
    callsite::{Callsite as _, DefaultCallsite},
    event::ParentKind,
    field::{FieldSet, Value},
    span::Id,
    Event, Kind, Level, Metadata,
};

static CALLSITE: DefaultCallsite = {
    static META: Metadata<'static> = Metadata::new(
        "parent_kind_test_event",
        "event_parent_kind",
        Level::INFO,
        None,
        None,
        None,
        FieldSet::new(&["message"], tracing_core::callsite::Identifier(&CALLSITE)),
        Kind::EVENT,
    );
    DefaultCallsite::new(&META)
};

/// The parent provenance of an event must be classified read-only into one of
/// three kinds: an explicit parent, the current (contextual) span, or no
/// parent (root). An explicit `None` parent must *not* fall back to the
/// current span.
#[test]
fn event_parent_kind_classification() {
    let meta = CALLSITE.metadata();
    let field = meta.fields().field("message").unwrap();
    let message = "hello";
    let values = [(&field, Some(&message as &dyn Value))];
    let value_set = meta.fields().value_set(&values);

    // A contextual event: its parent is the current span, if one is entered.
    let event = Event::new(meta, &value_set);
    assert_eq!(event.parent_kind(), ParentKind::Current);
    assert!(event.is_contextual());
    assert!(!event.is_root());
    assert_eq!(event.parent(), None);

    // An event with an explicitly-specified parent.
    let parent_id = Id::from_u64(42);
    let event = Event::new_child_of(Some(parent_id.clone()), meta, &value_set);
    assert_eq!(event.parent_kind(), ParentKind::Explicit(&parent_id));
    assert_eq!(event.parent(), Some(&parent_id));
    assert!(!event.is_contextual());
    assert!(!event.is_root());

    // An explicit `None` parent is a root; it must not fall back to the
    // current span.
    let event = Event::new_child_of(None, meta, &value_set);
    assert_eq!(event.parent_kind(), ParentKind::Root);
    assert!(event.is_root());
    assert!(!event.is_contextual());
    assert_eq!(event.parent(), None);
}
