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
    ///
    /// If `parent` is `None`, the event is explicitly a root ([`ParentKind::Root`]);
    /// it does *not* fall back to the current span.
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

    /// Returns a read-only classification of this event's parent provenance.
    ///
    /// This distinguishes the three possible sources of an event's parent:
    /// an [explicitly-specified] parent span, the [current] span (determined
    /// contextually), or [no parent] at all. Unlike [`Event::parent`], the
    /// returned [`ParentKind`] does not collapse the "contextual" and "root"
    /// cases into `None`, so observers can tell them apart without changing
    /// how the event is dispatched to subscribers.
    ///
    /// [explicitly-specified]: ParentKind::Explicit
    /// [current]: ParentKind::Current
    /// [no parent]: ParentKind::Root
    pub fn parent_kind(&self) -> ParentKind<'_> {
        match self.parent {
            Parent::Root => ParentKind::Root,
            Parent::Current => ParentKind::Current,
            Parent::Explicit(ref p) => ParentKind::Explicit(p),
        }
    }

    /// Returns true if the new event should be a root.
    pub fn is_root(&self) -> bool {
        matches!(self.parent_kind(), ParentKind::Root)
    }

    /// Returns true if the new event's parent should be determined based on the
    /// current context.
    ///
    /// If this is true and the current thread is currently inside a span, then
    /// that span should be the new event's parent. Otherwise, if the current
    /// thread is _not_ inside a span, then the new event will be the root of its
    /// own trace tree.
    pub fn is_contextual(&self) -> bool {
        matches!(self.parent_kind(), ParentKind::Current)
    }

    /// Returns the new event's explicitly-specified parent, if there is one.
    ///
    /// Otherwise (if the new event is a root or is a child of the current span),
    /// returns `None`.
    pub fn parent(&self) -> Option<&Id> {
        match self.parent_kind() {
            ParentKind::Explicit(p) => Some(p),
            _ => None,
        }
    }
}

/// A read-only classification of an [`Event`]'s parent provenance, returned by
/// [`Event::parent_kind`].
///
/// This classifies where an event's parent span comes from, without
/// exposing a way to mutate it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ParentKind<'a> {
    /// The event has an explicitly-specified parent span, identified by its
    /// [`Id`].
    ///
    /// Note that the [`Id`] is not guaranteed to refer to a span that the
    /// current [`Subscriber`] knows about: it may identify a span that was
    /// created by a *different* [`Dispatch`] (for example, if the span
    /// crossed a dispatcher boundary). Subscribers resolving the parent
    /// should treat such IDs as unknown rather than fabricating an
    /// association with an unrelated span.
    ///
    /// [`Subscriber`]: crate::Subscriber
    /// [`Dispatch`]: crate::Dispatch
    Explicit(&'a Id),

    /// The event's parent is determined based on the current context: if a
    /// span is currently entered on this thread, that span is the parent;
    /// otherwise, the event is the root of its own trace tree.
    Current,

    /// The event has no parent; it is the root of its own trace tree.
    ///
    /// This is the result of *explicitly* requesting no parent (e.g. via
    /// [`Event::new_child_of`] with a `None` parent). An explicit `None`
    /// parent never falls back to the current span.
    Root,
}
