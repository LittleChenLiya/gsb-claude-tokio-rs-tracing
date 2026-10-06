use core::fmt;

use alloc::vec::Vec;
use tracing_core::{
    callsite,
    field::{Field, Visit},
    span::{Attributes, Record},
    Metadata,
};

use super::RecordFields;

/// A plan for recording the fields of a span.
///
/// A `RecordPlan` tracks which of the fields declared by a span's
/// [`Metadata`] have been provided with values: fields that were initialized
/// when the span was constructed (via its [`Attributes`]) and fields that are
/// recorded afterwards (via [`Record`]s). A plan is generated from the span's
/// attributes with [`RecordPlan::new`] (or from the metadata alone with
/// [`RecordPlan::from_metadata`]) and updated with each subsequent [`Record`]
/// using [`RecordPlan::record`].
///
/// Fields are associated with the plan by their actual [`Field`] keys, which
/// carry the identity of the [callsite] that defined them, rather than by
/// field name. This means that two callsites which declare fields with the
/// same name remain independent: recording a value for one callsite's field
/// never marks the other callsite's same-named field as recorded, and
/// [`Record`]s belonging to a different callsite are ignored rather than
/// merged into the plan.
///
/// The plan only records the *source* of each field &mdash; its [`Field`] key
/// and the callsite's metadata &mdash; and never inspects, stores, or formats
/// the recorded *values*. Generating or updating a plan does not evaluate the
/// [`fmt::Debug`] or [`fmt::Display`] implementations of any recorded values.
/// Consequently, a `RecordPlan` cannot be used to snapshot field values (for
/// example, to include them in a failure report); this is deliberate, and
/// keeps plan generation cheap.
///
/// [callsite]: tracing_core::callsite
///
/// # Examples
///
/// ```
/// use tracing_core::{
///     callsite::Callsite,
///     field::Value,
///     metadata::Kind,
///     span::{Attributes, Record},
/// };
/// use tracing_subscriber::field::{FieldState, RecordPlan};
///
/// // A callsite for a span with two fields, `greeting` and `parting`:
/// let callsite = tracing::callsite! {
///     name: "my_span",
///     kind: Kind::SPAN,
///     fields: greeting, parting
/// };
/// let meta = callsite.metadata();
/// let greeting = meta.fields().field("greeting").unwrap();
/// let parting = meta.fields().field("parting").unwrap();
///
/// // The span is created with a value for `greeting` only:
/// let values = [(&greeting, Some(&"hello" as &dyn Value))];
/// let values = meta.fields().value_set(&values);
/// let attrs = Attributes::new(meta, &values);
///
/// let mut plan = RecordPlan::new(&attrs);
/// assert_eq!(plan.state(&greeting), Some(FieldState::Initialized));
/// assert_eq!(plan.state(&parting), Some(FieldState::Pending));
///
/// // Later, a value is recorded for `parting`:
/// let values = [(&parting, Some(&"goodbye" as &dyn Value))];
/// let values = meta.fields().value_set(&values);
/// plan.record(&Record::new(&values));
/// assert_eq!(plan.state(&parting), Some(FieldState::Recorded));
/// ```
#[derive(Clone)]
pub struct RecordPlan {
    /// The metadata of the callsite that declared the tracked fields.
    ///
    /// This is the *source* of every field in the plan. It is retained so
    /// that fields can be associated by callsite identity, and so that
    /// reports can describe where the fields came from. No field values are
    /// stored.
    meta: &'static Metadata<'static>,

    /// The recording state of each declared field, in declaration order.
    states: Vec<FieldState>,
}

/// The recording state of a single field tracked by a [`RecordPlan`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FieldState {
    /// No value has been recorded for the field yet.
    Pending,

    /// A value for the field was provided when the span was constructed.
    Initialized,

    /// A value for the field was recorded after the span was constructed.
    Recorded,
}

impl RecordPlan {
    /// Generates a recording plan for a new span described by `attrs`.
    ///
    /// Fields for which `attrs` contains a value are marked
    /// [`FieldState::Initialized`]; all other fields declared by the span's
    /// metadata start out [`FieldState::Pending`].
    ///
    /// Only the [`Field`] keys in `attrs` are read; the values themselves are
    /// never inspected.
    pub fn new(attrs: &Attributes<'_>) -> Self {
        let mut plan = Self::from_metadata(attrs.metadata());
        plan.mark_all(attrs, FieldState::Initialized);
        plan
    }

    /// Generates a recording plan from a span's `metadata`, with every
    /// declared field starting out [`FieldState::Pending`].
    pub fn from_metadata(meta: &'static Metadata<'static>) -> Self {
        Self {
            meta,
            states: alloc::vec![FieldState::Pending; meta.fields().len()],
        }
    }

    /// Updates the plan with a [`Record`] recorded for the span, marking each
    /// field it contains as [`FieldState::Recorded`].
    ///
    /// Each field is associated with the plan using its actual [`Field`] key,
    /// which carries the identity of the callsite that defined it. Fields
    /// that belong to a different callsite &mdash; including fields with the
    /// same name as one of this span's fields &mdash; are ignored rather than
    /// merged into the plan.
    ///
    /// Only the [`Field`] keys in `values` are read; the values themselves
    /// are never inspected.
    pub fn record(&mut self, values: &Record<'_>) {
        self.mark_all(values, FieldState::Recorded);
    }

    /// Returns the metadata of the span this plan was generated for.
    ///
    /// This is the source that every field in the plan is associated with.
    pub fn metadata(&self) -> &'static Metadata<'static> {
        self.meta
    }

    /// Returns an [`Identifier`] that uniquely identifies the callsite whose
    /// fields this plan tracks.
    ///
    /// [`Identifier`]: callsite::Identifier
    pub fn callsite(&self) -> callsite::Identifier {
        self.meta.callsite()
    }

    /// Returns the number of fields declared by the span's metadata.
    pub fn len(&self) -> usize {
        self.states.len()
    }

    /// Returns `true` if the span's metadata declares no fields.
    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }

    /// Returns `true` if `field` is declared by the callsite this plan
    /// tracks.
    ///
    /// Fields belonging to a different callsite are never contained in the
    /// plan, even if they have the same name as one of its fields.
    pub fn contains(&self, field: &Field) -> bool {
        field.callsite() == self.meta.callsite() && field.index() < self.states.len()
    }

    /// Returns the current [`FieldState`] of `field`, or `None` if `field`
    /// belongs to a different callsite than the one this plan tracks.
    pub fn state(&self, field: &Field) -> Option<FieldState> {
        if self.contains(field) {
            Some(self.states[field.index()])
        } else {
            None
        }
    }

    /// Returns an iterator over every declared field and its current
    /// [`FieldState`], in declaration order.
    pub fn fields(&self) -> impl Iterator<Item = (Field, FieldState)> + '_ {
        self.meta.fields().iter().zip(self.states.iter().copied())
    }

    /// Returns an iterator over the fields that no value has been recorded
    /// for yet.
    pub fn pending(&self) -> impl Iterator<Item = Field> + '_ {
        self.fields_with(FieldState::Pending)
    }

    /// Returns an iterator over the fields that were initialized when the
    /// span was constructed.
    pub fn initialized(&self) -> impl Iterator<Item = Field> + '_ {
        self.fields_with(FieldState::Initialized)
    }

    /// Returns an iterator over the fields that were recorded after the span
    /// was constructed.
    pub fn recorded(&self) -> impl Iterator<Item = Field> + '_ {
        self.fields_with(FieldState::Recorded)
    }

    fn fields_with(&self, state: FieldState) -> impl Iterator<Item = Field> + '_ {
        self.fields()
            .filter_map(move |(field, actual)| (actual == state).then_some(field))
    }

    fn mark_all<R>(&mut self, fields: &R, state: FieldState)
    where
        R: RecordFields,
    {
        fields.record(&mut MarkVisitor { plan: self, state });
    }

    fn mark(&mut self, field: &Field, state: FieldState) {
        // Associate the field with the plan by its *actual* `Field` key:
        // fields from another callsite are ignored, even if they have the
        // same name as one of this plan's fields.
        if self.contains(field) {
            if let Some(slot) = self.states.get_mut(field.index()) {
                *slot = state;
            }
        }
    }
}

impl fmt::Debug for RecordPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The report contains only the *source* of each field (its name and
        // the callsite that defined it) and its recording state. Recorded
        // values are never included, as they are not stored by the plan.
        f.debug_struct("RecordPlan")
            .field("span", &self.meta.name())
            .field("callsite", &self.meta.callsite())
            .field("fields", &FieldsReport(self))
            .finish()
    }
}

/// The `fmt::Debug` report of a plan's fields: names and states only.
struct FieldsReport<'a>(&'a RecordPlan);

impl fmt::Debug for FieldsReport<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map()
            .entries(self.0.fields().map(|(field, state)| (field.name(), state)))
            .finish()
    }
}

/// A visitor that marks the fields it visits in a [`RecordPlan`].
///
/// Only the [`Field`] keys are read; the values are discarded without being
/// inspected, so generating or updating a plan never evaluates the
/// `fmt::Debug` or `fmt::Display` implementation of a recorded value.
struct MarkVisitor<'a> {
    plan: &'a mut RecordPlan,
    state: FieldState,
}

impl Visit for MarkVisitor<'_> {
    fn record_debug(&mut self, field: &Field, _value: &dyn fmt::Debug) {
        self.plan.mark(field, self.state);
    }
}
