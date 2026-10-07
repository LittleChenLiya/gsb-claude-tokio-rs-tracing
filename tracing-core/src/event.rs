//! Events represent single points in time during the execution of a program.
use crate::parent::Parent;
use crate::span::Id;
use crate::{field, Metadata};

/// `Event`s represent single points in time where something occurred during the
/// execution of a program.
///
/// An `Event` can be compared to a log record in unstructured logging, but with
/// two key differences:
/// - `Event`s exist _within the context of a [span]_. Unlike log lines, they
///   may be located within the trace tree, allowing visibility into the
///   _temporal_ context in which the event occurred, as well as the source
///   code location.
/// - Like spans, `Event`s have structured key-value data known as _[fields]_,
///   which may include textual message. In general, a majority of the data
///   associated with an event should be in the event's fields rather than in
///   the textual message, as the fields are more structured.
///
/// [span]: super::span
/// [fields]: super::field
#[derive(Debug)]
pub struct Event<'a> {
    fields: &'a field::ValueSet<'a>,
    metadata: &'static Metadata<'static>,
    parent: Parent,
}

impl<'a> Event<'a> {
    /// Constructs a new `Event` with the specified metadata and set of values,
    /// and observes it with the current subscriber.
    pub fn dispatch(metadata: &'static Metadata<'static>, fields: &'a field::ValueSet<'_>) {
        let event = Event::new(metadata, fields);
        crate::dispatcher::get_default(|current| {
            current.event(&event);
        });
    }

    /// Returns a new `Event` in the current span, with the specified metadata
    /// and set of values.
    #[inline]
    pub fn new(metadata: &'static Metadata<'static>, fields: &'a field::ValueSet<'a>) -> Self {
        Event {
            fields,
            metadata,
            parent: Parent::Current,
        }
    }

    /// Returns a new `Event` as a child of the specified span, with the
    /// provided metadata and set of values.
    #[inline]
    pub fn new_child_of(
        parent: impl Into<Option<Id>>,
        metadata: &'static Metadata<'static>,
        fields: &'a field::ValueSet<'a>,
    ) -> Self {
        let parent = match parent.into() {
            Some(p) => Parent::Explicit(p),
            None => Parent::Root,
        };
        Event {
            fields,
            metadata,
            parent,
        }
    }

    /// Constructs a new `Event` with the specified metadata and set of values,
    /// and observes it with the current subscriber and an explicit parent.
    pub fn child_of(
        parent: impl Into<Option<Id>>,
        metadata: &'static Metadata<'static>,
        fields: &'a field::ValueSet<'_>,
    ) {
        let event = Self::new_child_of(parent, metadata, fields);
        crate::dispatcher::get_default(|current| {
            current.event(&event);
        });
    }

    /// Visits all the fields on this `Event` with the specified [visitor].
    ///
    /// [visitor]: super::field::Visit
    #[inline]
    pub fn record(&self, visitor: &mut dyn field::Visit) {
        self.fields.record(visitor);
    }

    /// Returns an iterator over the set of values on this `Event`.
    pub fn fields(&self) -> field::Iter {
        self.fields.field_set().iter()
    }

    /// Returns [metadata] describing this `Event`.
    ///
    /// [metadata]: super::Metadata
    pub fn metadata(&self) -> &'static Metadata<'static> {
        self.metadata
    }

    /// Returns true if the new event should be a root.
    pub fn is_root(&self) -> bool {
        matches!(self.parent, Parent::Root)
    }

    /// Returns true if the new event's parent should be determined based on the
    /// current context.
    ///
    /// If this is true and the current thread is currently inside a span, then
    /// that span should be the new event's parent. Otherwise, if the current
    /// thread is _not_ inside a span, then the new event will be the root of its
    /// own trace tree.
    pub fn is_contextual(&self) -> bool {
        matches!(self.parent, Parent::Current)
    }

    /// Returns the new event's explicitly-specified parent, if there is one.
    ///
    /// Otherwise (if the new event is a root or is a child of the current span),
    /// returns `None`.
    pub fn parent(&self) -> Option<&Id> {
        match self.parent {
            Parent::Explicit(ref p) => Some(p),
            _ => None,
        }
    }

    /// Returns a read-only classification of where this event's parent span
    /// comes from.
    ///
    /// This classifies the event's parent provenance as one of three cases:
    ///
    /// - [`ParentKind::Explicit`]: the event was constructed with an explicit
    ///   parent span ID (e.g. via [`Event::new_child_of`] with `Some(id)`),
    /// - [`ParentKind::Current`]: the event's parent is determined
    ///   contextually, based on the current span,
    /// - [`ParentKind::Root`]: the event has no parent. Note that an event
    ///   constructed with an *explicit* `None` parent (e.g. via
    ///   [`Event::new_child_of`] with `None`) is a root, and does **not** fall
    ///   back to the current span.
    ///
    /// Unlike [`Event::parent`], this does not erase the distinction between
    /// a root event and a contextually-parented event.
    pub fn parent_kind(&self) -> ParentKind<'_> {
        match self.parent {
            Parent::Root => ParentKind::Root,
            Parent::Current => ParentKind::Current,
            Parent::Explicit(ref id) => ParentKind::Explicit(id),
        }
    }
}

/// A read-only classification of an [`Event`]'s parent provenance.
///
/// Returned by [`Event::parent_kind`]. This distinguishes the three ways an
/// event's parent span may be determined: an explicitly-provided span ID, the
/// current span, or no parent at all (a root event).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ParentKind<'a> {
    /// The event is a root: it has no parent.
    ///
    /// An event whose parent was *explicitly* set to `None` is a root, and
    /// must not fall back to the current span.
    Root,
    /// The event's parent is determined contextually: if the current thread
    /// is inside a span, that span is the event's parent; otherwise, the
    /// event is the root of its own trace tree.
    Current,
    /// The event has an explicitly-specified parent, identified by the
    /// contained span [`Id`].
    ///
    /// Note that the contained `Id` is only guaranteed to be meaningful to
    /// the [`Subscriber`] that produced it; subscribers that do not recognize
    /// the ID (for example, because the parent span was created by a
    /// different [`Dispatch`]er) should treat the parent as unknown rather
    /// than fabricating an association with some other span.
    ///
    /// [`Subscriber`]: crate::subscriber::Subscriber
    /// [`Dispatch`]: crate::dispatcher::Dispatch
    Explicit(&'a Id),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::callsite::{Callsite, Identifier};
    use crate::metadata::Kind;
    use crate::span;
    use crate::subscriber::Interest;

    struct TestCallsite;
    static TEST_CALLSITE: TestCallsite = TestCallsite;

    impl Callsite for TestCallsite {
        fn set_interest(&self, _: Interest) {}
        fn metadata(&self) -> &Metadata<'_> {
            &TEST_META
        }
    }

    static TEST_META: Metadata<'static> = Metadata::new(
        "event_parent_kind_test",
        "tracing_core",
        crate::Level::INFO,
        None,
        None,
        None,
        field::FieldSet::new(&[], Identifier(&TEST_CALLSITE)),
        Kind::EVENT,
    );

    fn with_event(f: impl FnOnce(Event<'_>)) {
        let fields = TEST_META.fields();
        let values = [];
        let valueset = fields.value_set(&values);
        f(Event::new(&TEST_META, &valueset));
    }

    fn with_event_child_of(parent: Option<Id>, f: impl FnOnce(Event<'_>)) {
        let fields = TEST_META.fields();
        let values = [];
        let valueset = fields.value_set(&values);
        f(Event::new_child_of(parent, &TEST_META, &valueset));
    }

    #[test]
    fn parent_kind_current_by_default() {
        with_event(|event| {
            assert_eq!(event.parent_kind(), ParentKind::Current);
            assert!(event.is_contextual());
            assert!(!event.is_root());
            assert_eq!(event.parent(), None);
        });
    }

    #[test]
    fn parent_kind_explicit() {
        let id = span::Id::from_u64(42);
        with_event_child_of(Some(id.clone()), |event| {
            assert_eq!(event.parent_kind(), ParentKind::Explicit(&id));
            assert_eq!(event.parent(), Some(&id));
            assert!(!event.is_contextual());
            assert!(!event.is_root());
        });
    }

    #[test]
    fn parent_kind_explicit_none_is_root_not_current() {
        // An explicit `None` parent must be classified as a root, and must
        // *not* fall back to the current span.
        with_event_child_of(None, |event| {
            assert_eq!(event.parent_kind(), ParentKind::Root);
            assert!(event.is_root());
            assert!(!event.is_contextual());
            assert_eq!(event.parent(), None);
        });
    }
}
