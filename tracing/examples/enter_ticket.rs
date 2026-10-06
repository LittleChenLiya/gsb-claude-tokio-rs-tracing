//! A small example of moving a span *entry credential* between threads.
//!
//! A `tracing::span::EnterTicket` binds a span's identity and the subscriber
//! that created it. The ticket (unlike an `Entered`/`EnteredSpan` guard) is
//! `Send`, so it can be handed to another thread, which re-enters the span
//! locally and exits it by dropping its own guard. If the ticket is used
//! while a *different* subscriber is the default, entering is rejected.
//!
//! Run with:
//!
//! ```text
//! cargo run -p tracing --example enter_ticket
//! ```

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
    thread,
};

use tracing::{
    dispatcher::{self, Dispatch},
    info, info_span,
    span::Id,
    Metadata, Subscriber,
};
use tracing_core::span::{Attributes, Current, Record};

/// A minimal subscriber that prints span enter/exit activity, tagged with
/// the name of the thread it happened on.
struct Printer {
    spans: Mutex<HashMap<u64, &'static Metadata<'static>>>,
    next_id: AtomicU64,
}

thread_local! {
    static SPAN_STACK: std::cell::RefCell<Vec<u64>> = const { std::cell::RefCell::new(Vec::new()) };
}

impl Printer {
    fn new() -> Self {
        Printer {
            spans: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        }
    }

    fn thread() -> String {
        thread::current().name().unwrap_or("<unnamed>").to_string()
    }
}

impl Subscriber for Printer {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, attrs: &Attributes<'_>) -> Id {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.spans.lock().unwrap().insert(id, attrs.metadata());
        Id::from_u64(id)
    }

    fn record(&self, _: &Id, _: &Record<'_>) {}

    fn record_follows_from(&self, _: &Id, _: &Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        println!("[{}] event: {:?}", Self::thread(), event);
    }

    fn enter(&self, id: &Id) {
        SPAN_STACK.with(|stack| stack.borrow_mut().push(id.into_u64()));
        println!("[{}] enter span #{}", Self::thread(), id.into_u64());
    }

    fn exit(&self, id: &Id) {
        SPAN_STACK.with(|stack| {
            stack.borrow_mut().pop();
        });
        println!("[{}] exit span #{}", Self::thread(), id.into_u64());
    }

    fn current_span(&self) -> Current {
        SPAN_STACK.with(|stack| {
            stack
                .borrow()
                .last()
                .and_then(|id| {
                    self.spans
                        .lock()
                        .unwrap()
                        .get(id)
                        .map(|meta| Current::new(Id::from_u64(*id), meta))
                })
                .unwrap_or_else(Current::none)
        })
    }
}

fn main() {
    let dispatch = Dispatch::new(Printer::new());

    dispatcher::with_default(&dispatch, || {
        let span = info_span!("shared_job");
        let ticket = span
            .enter_ticket()
            .expect("the span is enabled, so it yields a ticket");

        // The ticket is `Send`, so it can be shared with worker threads.
        // Each worker re-enters the span on *its own* thread and exits it by
        // dropping its own guard.
        let workers: Vec<_> = (0..2)
            .map(|n| {
                let dispatch = dispatch.clone();
                let ticket = ticket.clone();
                thread::Builder::new()
                    .name(format!("worker-{n}"))
                    .spawn(move || {
                        dispatcher::with_default(&dispatch, || {
                            let _entered = ticket
                                .enter()
                                .expect("the subscriber matches, so the ticket is accepted");
                            info!("doing a share of the work");
                        });
                    })
                    .unwrap()
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }

        // If the default subscriber is replaced, the old ticket is rejected
        // instead of entering the span on the wrong subscriber.
        let other = Dispatch::new(Printer::new());
        dispatcher::with_default(&other, || match ticket.enter() {
            Err(error) => println!("[main] ticket rejected, as expected: {error}"),
            Ok(_) => println!("[main] unexpected: ticket was accepted!"),
        });
    });

    // The ticket itself can be sent to and shared between threads freely;
    // only the entered guard is thread-local.
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<tracing::span::EnterTicket>();
}
