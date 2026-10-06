// These tests require the thread-local scoped dispatcher, which only works
// when we have a standard library.
#![cfg(feature = "std")]

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread::{self, ThreadId},
};

use tracing::{
    dispatcher::{self, Dispatch},
    span::{EnterTicket, Id},
    Metadata, Span, Subscriber,
};
use tracing_core::span::{Attributes, Current, Record};

/// A test subscriber that records `enter`/`exit` calls (with the thread they
/// occurred on) and tracks a per-thread span stack, so tests can observe the
/// current span context on each thread.
#[derive(Clone)]
struct Recorder {
    state: Arc<State>,
}

struct State {
    events: Mutex<Vec<Event>>,
    spans: Mutex<HashMap<u64, &'static Metadata<'static>>>,
    next_id: AtomicU64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Event {
    Enter(u64, ThreadId),
    Exit(u64, ThreadId),
}

thread_local! {
    /// The span stack of the current thread, maintained by `Recorder`.
    static SPAN_STACK: std::cell::RefCell<Vec<u64>> = const { std::cell::RefCell::new(Vec::new()) };
}

impl Recorder {
    fn new() -> (Self, Dispatch) {
        let recorder = Recorder {
            state: Arc::new(State {
                events: Mutex::new(Vec::new()),
                spans: Mutex::new(HashMap::new()),
                next_id: AtomicU64::new(1),
            }),
        };
        let dispatch = Dispatch::new(recorder.clone());
        (recorder, dispatch)
    }

    fn events(&self) -> Vec<Event> {
        self.state.events.lock().unwrap().clone()
    }
}

impl Subscriber for Recorder {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, attrs: &Attributes<'_>) -> Id {
        let id = self.state.next_id.fetch_add(1, Ordering::Relaxed);
        self.state
            .spans
            .lock()
            .unwrap()
            .insert(id, attrs.metadata());
        Id::from_u64(id)
    }

    fn record(&self, _: &Id, _: &Record<'_>) {}

    fn record_follows_from(&self, _: &Id, _: &Id) {}

    fn event(&self, _: &tracing::Event<'_>) {}

    fn enter(&self, id: &Id) {
        let id = id.into_u64();
        SPAN_STACK.with(|stack| stack.borrow_mut().push(id));
        self.state
            .events
            .lock()
            .unwrap()
            .push(Event::Enter(id, thread::current().id()));
    }

    fn exit(&self, id: &Id) {
        let id = id.into_u64();
        SPAN_STACK.with(|stack| {
            let exited = stack.borrow_mut().pop();
            assert_eq!(exited, Some(id), "exited span must be current");
        });
        self.state
            .events
            .lock()
            .unwrap()
            .push(Event::Exit(id, thread::current().id()));
    }

    fn current_span(&self) -> Current {
        SPAN_STACK.with(|stack| {
            let id = stack.borrow().last().copied();
            id.and_then(|id| {
                self.state
                    .spans
                    .lock()
                    .unwrap()
                    .get(&id)
                    .map(|meta| Current::new(Id::from_u64(id), meta))
            })
            .unwrap_or_else(Current::none)
        })
    }
}

/// Enters the span via `ticket` on a fresh thread where `dispatch` is the
/// default subscriber, asserting that the span context is entered and then
/// restored on that thread.
fn enter_on_thread(dispatch: Dispatch, ticket: EnterTicket, span_id: Id) -> ThreadId {
    thread::spawn(move || {
        dispatcher::with_default(&dispatch, || {
            // Before entering, the span context on this thread is empty.
            assert!(
                Span::current().is_none(),
                "fresh thread must not start inside the span"
            );

            let entered = ticket
                .enter()
                .expect("ticket must be accepted by the subscriber that created the span");

            // While the guard is held, the span is the current span.
            assert_eq!(
                Span::current().id(),
                Some(span_id),
                "entered span must be the current span"
            );

            drop(entered);

            // After the guard is dropped, the previous context is restored.
            assert!(
                Span::current().is_none(),
                "exiting the span must restore the prior context"
            );
        });

        thread::current().id()
    })
    .join()
    .expect("worker thread panicked")
}

#[test]
fn two_threads_enter_the_same_span_in_turn() {
    let (recorder, dispatch) = Recorder::new();

    dispatcher::with_default(&dispatch, || {
        let span = tracing::info_span!("shared_span");
        let span_id = span.id().expect("span must be enabled");
        let ticket = span
            .enter_ticket()
            .expect("enabled span must yield a ticket");

        // Two threads enter the *same* span, one after another. Each thread
        // re-enters via the ticket and exits by dropping its own guard; no
        // guard is ever moved between threads.
        let thread_a = enter_on_thread(dispatch.clone(), ticket.clone(), span_id.clone());
        let thread_b = enter_on_thread(dispatch.clone(), ticket, span_id.clone());

        assert_ne!(thread_a, thread_b, "must be two distinct threads");
        assert_ne!(thread_a, thread::current().id());

        // Each thread performed exactly one enter and one exit of the span,
        // on itself, in order.
        let id = span_id.into_u64();
        assert_eq!(
            recorder.events(),
            vec![
                Event::Enter(id, thread_a),
                Event::Exit(id, thread_a),
                Event::Enter(id, thread_b),
                Event::Exit(id, thread_b),
            ],
            "each thread must enter and exit the span exactly once, in turn"
        );
    });
}

#[test]
fn ticket_is_rejected_after_subscriber_is_replaced() {
    let (recorder_a, dispatch_a) = Recorder::new();
    let (_recorder_b, dispatch_b) = Recorder::new();

    // Create the span and its ticket while subscriber A is the default.
    let ticket = dispatcher::with_default(&dispatch_a, || {
        tracing::info_span!("shared_span")
            .enter_ticket()
            .expect("enabled span must yield a ticket")
    });

    // After the default subscriber is replaced with subscriber B, the old
    // ticket must be explicitly rejected...
    dispatcher::with_default(&dispatch_b, || {
        let err = ticket
            .enter()
            .expect_err("ticket must be rejected under a different subscriber");
        assert!(err.to_string().contains("does not match"));
    });

    // ...and it must also be rejected when no default subscriber is set.
    ticket
        .enter()
        .expect_err("ticket must be rejected with no default subscriber");

    // The rejected attempts must not have entered the span on subscriber A.
    assert!(
        recorder_a.events().is_empty(),
        "a rejected ticket must not enter the span"
    );

    // The ticket still works with the subscriber that created the span.
    dispatcher::with_default(&dispatch_a, || {
        let entered = ticket
            .enter()
            .expect("ticket must still be accepted by the original subscriber");
        drop(entered);
    });
    let this_thread = thread::current().id();
    assert_eq!(
        recorder_a.events(),
        vec![Event::Enter(1, this_thread), Event::Exit(1, this_thread)],
    );
}

#[test]
fn disabled_spans_do_not_yield_tickets() {
    assert!(Span::none().enter_ticket().is_none());
}
