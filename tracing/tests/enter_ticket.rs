// These tests require the thread-local scoped dispatcher, which only works
// when we have a standard library.
#![cfg(feature = "std")]

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    thread::{self, ThreadId},
};

use tracing::{info_span, Dispatch, Span};
use tracing_core::{
    span::{Attributes, Current, Id, Record},
    Event, Metadata, Subscriber,
};

/// A test subscriber that records every `enter`/`exit` it receives, along
/// with the thread it was called on, and tracks a per-thread span stack so
/// that tests can observe each thread's current span via `Span::current`.
#[derive(Clone, Default)]
struct ThreadStacks {
    state: Arc<Mutex<State>>,
}

#[derive(Default)]
struct State {
    next_id: u64,
    spans: HashMap<u64, &'static Metadata<'static>>,
    stacks: HashMap<ThreadId, Vec<u64>>,
    events: Vec<(ThreadId, SpanEvent)>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SpanEvent {
    Enter(u64),
    Exit(u64),
}

impl ThreadStacks {
    fn events(&self) -> Vec<(ThreadId, SpanEvent)> {
        self.state.lock().unwrap().events.clone()
    }

    fn stack_of(&self, thread: ThreadId) -> Vec<u64> {
        self.state
            .lock()
            .unwrap()
            .stacks
            .get(&thread)
            .cloned()
            .unwrap_or_default()
    }
}

impl Subscriber for ThreadStacks {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, attrs: &Attributes<'_>) -> Id {
        let mut state = self.state.lock().unwrap();
        state.next_id += 1;
        let id = state.next_id;
        state.spans.insert(id, attrs.metadata());
        Id::from_u64(id)
    }

    fn record(&self, _: &Id, _: &Record<'_>) {}

    fn record_follows_from(&self, _: &Id, _: &Id) {}

    fn event(&self, _: &Event<'_>) {}

    fn enter(&self, id: &Id) {
        let mut state = self.state.lock().unwrap();
        let thread = thread::current().id();
        state.events.push((thread, SpanEvent::Enter(id.into_u64())));
        state.stacks.entry(thread).or_default().push(id.into_u64());
    }

    fn exit(&self, id: &Id) {
        let mut state = self.state.lock().unwrap();
        let thread = thread::current().id();
        state.events.push((thread, SpanEvent::Exit(id.into_u64())));
        let popped = state.stacks.entry(thread).or_default().pop();
        assert_eq!(
            popped,
            Some(id.into_u64()),
            "an exited span must be the innermost span entered on this thread",
        );
    }

    fn current_span(&self) -> Current {
        let state = self.state.lock().unwrap();
        let thread = thread::current().id();
        match state.stacks.get(&thread).and_then(|stack| stack.last()) {
            Some(&id) => Current::new(Id::from_u64(id), state.spans[&id]),
            None => Current::none(),
        }
    }
}

/// Two threads sequentially enter the same span via an `EnterTicket`. Each
/// thread re-enters the span on itself and exits it from itself; the entry
/// guard is never moved between threads.
#[test]
fn two_threads_sequentially_enter_the_same_span() {
    let subscriber = ThreadStacks::default();
    let dispatch = Dispatch::new(subscriber.clone());

    // Create the span and its entry ticket while `dispatch` is the default.
    let span = tracing::dispatcher::with_default(&dispatch, || info_span!("shared_span"));
    let span_id = span.id().expect("the span is enabled");
    let ticket = span.enter_ticket();
    assert_eq!(ticket.id(), Some(span_id.clone()));

    // First thread: enter the shared span via the ticket, nested inside a
    // thread-local outer span, so that context restoration is observable.
    let (ticket, outer_id) = {
        let dispatch = dispatch.clone();
        let span_id = span_id.clone();
        thread::spawn(move || {
            tracing::dispatcher::with_default(&dispatch, || {
                let outer = info_span!("outer");
                let outer_id = outer.id().expect("the outer span is enabled");
                let outer_guard = outer.enter();

                // The ticket re-enters the shared span on *this* thread.
                let guard = ticket
                    .enter()
                    .expect("the issuing subscriber is the default");
                assert_eq!(guard.id(), Some(span_id.clone()));
                assert_eq!(
                    Span::current().id().as_ref(),
                    Some(&span_id),
                    "the shared span is the current span on thread 1",
                );

                // Dropping the guard on this thread exits the span here and
                // restores the previous context.
                drop(guard);
                assert_eq!(
                    Span::current().id().as_ref(),
                    Some(&outer_id),
                    "exiting the ticket guard restores the outer span",
                );

                drop(outer_guard);
                assert_eq!(Span::current().id(), None);

                // Return the ticket so that the next thread can use it.
                (ticket, outer_id)
            })
        })
        .join()
        .unwrap()
    };

    // Second thread: enter the same span via the same ticket.
    {
        let dispatch = dispatch.clone();
        let span_id = span_id.clone();
        thread::spawn(move || {
            tracing::dispatcher::with_default(&dispatch, || {
                assert_eq!(Span::current().id(), None);

                let guard = ticket
                    .enter()
                    .expect("the issuing subscriber is the default");
                assert_eq!(
                    Span::current().id().as_ref(),
                    Some(&span_id),
                    "the shared span is the current span on thread 2",
                );

                drop(guard);
                assert_eq!(Span::current().id(), None);
            })
        })
        .join()
        .unwrap();
    }

    // The threads ran sequentially, so the recorded enter/exit stream is
    // deterministic: thread 1 entered its outer span, entered and exited the
    // shared span, and exited the outer span; thread 2 entered and exited
    // the shared span.
    let span_id = span_id.into_u64();
    let outer_id = outer_id.into_u64();
    let events = subscriber.events();
    assert_eq!(events.len(), 6, "unexpected events: {:?}", events);
    let thread1 = events[0].0;
    let thread2 = events[5].0;
    assert_ne!(thread1, thread2, "the two threads must be distinct");
    assert_eq!(
        events,
        vec![
            (thread1, SpanEvent::Enter(outer_id)),
            (thread1, SpanEvent::Enter(span_id)),
            (thread1, SpanEvent::Exit(span_id)),
            (thread1, SpanEvent::Exit(outer_id)),
            (thread2, SpanEvent::Enter(span_id)),
            (thread2, SpanEvent::Exit(span_id)),
        ],
    );

    // Each thread's span stack is empty again: every thread exited exactly
    // what it entered, restoring its prior context.
    assert!(subscriber.stack_of(thread1).is_empty());
    assert!(subscriber.stack_of(thread2).is_empty());
}

/// Once the default subscriber has been replaced, a ticket issued by the old
/// subscriber is explicitly rejected, and the span is not entered.
#[test]
fn ticket_is_rejected_when_the_subscriber_is_replaced() {
    let subscriber_a = ThreadStacks::default();
    let dispatch_a = Dispatch::new(subscriber_a.clone());
    let subscriber_b = ThreadStacks::default();
    let dispatch_b = Dispatch::new(subscriber_b.clone());

    // Issue a ticket while subscriber A is the default.
    let span = tracing::dispatcher::with_default(&dispatch_a, || info_span!("span_from_a"));
    let ticket = span.enter_ticket();

    // While subscriber A remains the default, the ticket is honored.
    tracing::dispatcher::with_default(&dispatch_a, || {
        let guard = ticket
            .enter()
            .expect("the issuing subscriber is the default");
        drop(guard);
    });
    assert_eq!(subscriber_a.events().len(), 2);

    // After the default subscriber is replaced by subscriber B, the old
    // ticket is explicitly rejected and nothing is entered.
    tracing::dispatcher::with_default(&dispatch_b, || {
        let err = ticket
            .enter()
            .expect_err("the ticket's subscriber has been replaced");
        assert!(
            err.to_string()
                .contains("not the current default subscriber"),
            "unexpected error message: {}",
            err,
        );
    });
    assert!(
        subscriber_b.events().is_empty(),
        "subscriber B must never see the span",
    );
    assert_eq!(
        subscriber_a.events().len(),
        2,
        "no further enter/exit may be recorded",
    );

    // With no default subscriber at all, the ticket is also rejected.
    ticket
        .enter()
        .expect_err("no default subscriber is set on this thread");
}

/// A ticket for a disabled span enters as a no-op, like `Span::enter`.
#[test]
fn disabled_span_ticket_enters_as_a_noop() {
    // No subscriber is the default on this thread, so the span is disabled.
    let span = info_span!("disabled_span");
    assert!(span.is_disabled());

    let ticket = span.enter_ticket();
    assert_eq!(ticket.id(), None);

    let guard = ticket
        .enter()
        .expect("a disabled span's ticket enters as a no-op");
    assert_eq!(guard.id(), None);
}

/// The ticket --- unlike the entry guard --- may be sent across threads.
#[test]
fn ticket_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<tracing::span::EnterTicket>();
}
