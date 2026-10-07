use super::*;
use tracing_core::subscriber::NoSubscriber;

#[derive(Debug)]
pub(crate) struct NopLayer;
impl<S: Subscriber> Layer<S> for NopLayer {}

#[allow(dead_code)]
struct NopLayer2;
impl<S: Subscriber> Layer<S> for NopLayer2 {}

/// A layer that holds a string.
///
/// Used to test that pointers returned by downcasting are actually valid.
struct StringLayer(&'static str);
impl<S: Subscriber> Layer<S> for StringLayer {}
struct StringLayer2(&'static str);
impl<S: Subscriber> Layer<S> for StringLayer2 {}

struct StringLayer3(&'static str);
impl<S: Subscriber> Layer<S> for StringLayer3 {}

pub(crate) struct StringSubscriber(&'static str);

impl Subscriber for StringSubscriber {
    fn register_callsite(&self, _: &'static Metadata<'static>) -> Interest {
        Interest::never()
    }

    fn enabled(&self, _: &Metadata<'_>) -> bool {
        false
    }

    fn new_span(&self, _: &span::Attributes<'_>) -> span::Id {
        span::Id::from_u64(1)
    }

    fn record(&self, _: &span::Id, _: &span::Record<'_>) {}
    fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}
    fn event(&self, _: &Event<'_>) {}
    fn enter(&self, _: &span::Id) {}
    fn exit(&self, _: &span::Id) {}
}

fn assert_subscriber(_s: impl Subscriber) {}
fn assert_layer<S: Subscriber>(_l: &impl Layer<S>) {}

#[test]
fn layer_is_subscriber() {
    let s = NopLayer.with_subscriber(NoSubscriber::default());
    assert_subscriber(s)
}

#[test]
fn two_layers_are_subscriber() {
    let s = NopLayer
        .and_then(NopLayer)
        .with_subscriber(NoSubscriber::default());
    assert_subscriber(s)
}

#[test]
fn three_layers_are_subscriber() {
    let s = NopLayer
        .and_then(NopLayer)
        .and_then(NopLayer)
        .with_subscriber(NoSubscriber::default());
    assert_subscriber(s)
}

#[test]
fn three_layers_are_layer() {
    let layers = NopLayer.and_then(NopLayer).and_then(NopLayer);
    assert_layer(&layers);
    let _ = layers.with_subscriber(NoSubscriber::default());
}

#[test]
#[cfg(feature = "alloc")]
fn box_layer_is_layer() {
    use alloc::boxed::Box;
    let l: Box<dyn Layer<NoSubscriber> + Send + Sync> = Box::new(NopLayer);
    assert_layer(&l);
    l.with_subscriber(NoSubscriber::default());
}

#[test]
fn downcasts_to_subscriber() {
    let s = NopLayer
        .and_then(NopLayer)
        .and_then(NopLayer)
        .with_subscriber(StringSubscriber("subscriber"));
    let subscriber =
        <dyn Subscriber>::downcast_ref::<StringSubscriber>(&s).expect("subscriber should downcast");
    assert_eq!(subscriber.0, "subscriber");
}

#[test]
fn downcasts_to_layer() {
    let s = StringLayer("layer_1")
        .and_then(StringLayer2("layer_2"))
        .and_then(StringLayer3("layer_3"))
        .with_subscriber(NoSubscriber::default());
    let layer = <dyn Subscriber>::downcast_ref::<StringLayer>(&s).expect("layer 1 should downcast");
    assert_eq!(layer.0, "layer_1");
    let layer =
        <dyn Subscriber>::downcast_ref::<StringLayer2>(&s).expect("layer 2 should downcast");
    assert_eq!(layer.0, "layer_2");
    let layer =
        <dyn Subscriber>::downcast_ref::<StringLayer3>(&s).expect("layer 3 should downcast");
    assert_eq!(layer.0, "layer_3");
}

#[cfg(all(feature = "registry", feature = "std"))]
mod registry_tests {
    use std::dbg;

    use super::*;
    use crate::registry::LookupSpan;

    #[test]
    fn context_event_span() {
        use std::sync::{Arc, Mutex};
        let last_event_span = Arc::new(Mutex::new(None));

        struct RecordingLayer {
            last_event_span: Arc<Mutex<Option<&'static str>>>,
        }

        impl<S> Layer<S> for RecordingLayer
        where
            S: Subscriber + for<'lookup> LookupSpan<'lookup>,
        {
            fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
                let span = ctx.event_span(event);
                *self.last_event_span.lock().unwrap() = span.map(|s| s.name());
            }
        }

        tracing::subscriber::with_default(
            crate::registry().with(RecordingLayer {
                last_event_span: last_event_span.clone(),
            }),
            || {
                tracing::info!("no span");
                assert_eq!(*last_event_span.lock().unwrap(), None);

                let parent = tracing::info_span!("explicit");
                tracing::info!(parent: &parent, "explicit span");
                assert_eq!(*last_event_span.lock().unwrap(), Some("explicit"));

                let _guard = tracing::info_span!("contextual").entered();
                tracing::info!("contextual span");
                assert_eq!(*last_event_span.lock().unwrap(), Some("contextual"));
            },
        );
    }

    #[test]
    fn context_event_parent() {
        use crate::layer::EventParent;
        use std::sync::{Arc, Mutex};

        /// A small in-memory fixture recording how each received event's
        /// parent was classified.
        #[derive(Debug, Clone, PartialEq, Eq)]
        enum Recorded {
            Root,
            Current(Option<&'static str>),
            Explicit(&'static str),
            Unknown,
        }

        let events = Arc::new(Mutex::new(Vec::new()));

        struct RecordingLayer {
            events: Arc<Mutex<Vec<Recorded>>>,
        }

        impl<S> Layer<S> for RecordingLayer
        where
            S: Subscriber + for<'lookup> LookupSpan<'lookup>,
        {
            fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
                let recorded = match ctx.event_parent(event) {
                    EventParent::Root => Recorded::Root,
                    EventParent::Current(current) => {
                        Recorded::Current(current.map(|span| span.name()))
                    }
                    EventParent::Explicit(span) => Recorded::Explicit(span.name()),
                    EventParent::Unknown(_) => Recorded::Unknown,
                };
                self.events.lock().unwrap().push(recorded);
            }
        }

        /// A subscriber standing in for a foreign `Dispatch`er: spans it
        /// creates get IDs that the registry under test will never know.
        struct ForeignSubscriber;
        impl Subscriber for ForeignSubscriber {
            fn enabled(&self, _: &Metadata<'_>) -> bool {
                true
            }
            fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
                tracing::span::Id::from_u64(u64::MAX)
            }
            fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
            fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
            fn event(&self, _: &Event<'_>) {}
            fn enter(&self, _: &tracing::span::Id) {}
            fn exit(&self, _: &tracing::span::Id) {}
        }

        // Create a span owned by a *different* dispatcher. Its ID is unknown
        // to the registry under test below.
        let foreign =
            tracing::subscriber::with_default(ForeignSubscriber, || tracing::info_span!("foreign"));

        tracing::subscriber::with_default(
            crate::registry().with(RecordingLayer {
                events: events.clone(),
            }),
            || {
                let _guard = tracing::info_span!("outer").entered();
                let explicit = tracing::info_span!("explicit");

                // One event of each parent kind, in order, inside `outer`:
                tracing::info!("contextual parent");
                tracing::info!(parent: &explicit, "explicit parent");
                tracing::info!(parent: None, "explicit root");
                // ...plus an event whose parent belongs to another dispatcher:
                tracing::info!(parent: &foreign, "foreign parent");
            },
        );

        // All four events must have been dispatched to the subscriber
        // unchanged, and each parent provenance must be classified correctly.
        // In particular, the explicit root must not fall back to the current
        // span, and the foreign parent must be reported as unknown rather
        // than being associated with a fabricated span.
        let events = events.lock().unwrap();
        assert_eq!(
            events.as_slice(),
            &[
                Recorded::Current(Some("outer")),
                Recorded::Explicit("explicit"),
                Recorded::Root,
                Recorded::Unknown,
            ],
        );
    }

    /// Tests for how max-level hints are calculated when combining layers
    /// with and without per-layer filtering.
    mod max_level_hints {

        use super::*;
        use crate::filter::*;

        #[test]
        fn mixed_with_unfiltered() {
            let subscriber = crate::registry()
                .with(NopLayer)
                .with(NopLayer.with_filter(LevelFilter::INFO));
            assert_eq!(subscriber.max_level_hint(), None);
        }

        #[test]
        fn mixed_with_unfiltered_layered() {
            let subscriber = crate::registry().with(NopLayer).with(
                NopLayer
                    .with_filter(LevelFilter::INFO)
                    .and_then(NopLayer.with_filter(LevelFilter::TRACE)),
            );
            assert_eq!(dbg!(subscriber).max_level_hint(), None);
        }

        #[test]
        fn mixed_interleaved() {
            let subscriber = crate::registry()
                .with(NopLayer)
                .with(NopLayer.with_filter(LevelFilter::INFO))
                .with(NopLayer)
                .with(NopLayer.with_filter(LevelFilter::INFO));
            assert_eq!(dbg!(subscriber).max_level_hint(), None);
        }

        #[test]
        fn mixed_layered() {
            let subscriber = crate::registry()
                .with(NopLayer.with_filter(LevelFilter::INFO).and_then(NopLayer))
                .with(NopLayer.and_then(NopLayer.with_filter(LevelFilter::INFO)));
            assert_eq!(dbg!(subscriber).max_level_hint(), None);
        }

        #[test]
        fn plf_only_unhinted() {
            let subscriber = crate::registry()
                .with(NopLayer.with_filter(LevelFilter::INFO))
                .with(NopLayer.with_filter(filter_fn(|_| true)));
            assert_eq!(dbg!(subscriber).max_level_hint(), None);
        }

        #[test]
        fn plf_only_unhinted_nested_outer() {
            // if a nested tree of per-layer filters has an _outer_ filter with
            // no max level hint, it should return `None`.
            let subscriber = crate::registry()
                .with(
                    NopLayer
                        .with_filter(LevelFilter::INFO)
                        .and_then(NopLayer.with_filter(LevelFilter::WARN)),
                )
                .with(
                    NopLayer
                        .with_filter(filter_fn(|_| true))
                        .and_then(NopLayer.with_filter(LevelFilter::DEBUG)),
                );
            assert_eq!(dbg!(subscriber).max_level_hint(), None);
        }

        #[test]
        fn plf_only_unhinted_nested_inner() {
            // If a nested tree of per-layer filters has an _inner_ filter with
            // no max-level hint, but the _outer_ filter has a max level hint,
            // it should pick the outer hint. This is because the outer filter
            // will disable the spans/events before they make it to the inner
            // filter.
            let subscriber = dbg!(crate::registry().with(
                NopLayer
                    .with_filter(filter_fn(|_| true))
                    .and_then(NopLayer.with_filter(filter_fn(|_| true)))
                    .with_filter(LevelFilter::INFO),
            ));
            assert_eq!(dbg!(subscriber).max_level_hint(), Some(LevelFilter::INFO));
        }

        #[test]
        fn unhinted_nested_inner() {
            let subscriber = dbg!(crate::registry()
                .with(NopLayer.and_then(NopLayer).with_filter(LevelFilter::INFO))
                .with(
                    NopLayer
                        .with_filter(filter_fn(|_| true))
                        .and_then(NopLayer.with_filter(filter_fn(|_| true)))
                        .with_filter(LevelFilter::WARN),
                ));
            assert_eq!(dbg!(subscriber).max_level_hint(), Some(LevelFilter::INFO));
        }

        #[test]
        fn unhinted_nested_inner_mixed() {
            let subscriber = dbg!(crate::registry()
                .with(
                    NopLayer
                        .and_then(NopLayer.with_filter(filter_fn(|_| true)))
                        .with_filter(LevelFilter::INFO)
                )
                .with(
                    NopLayer
                        .with_filter(filter_fn(|_| true))
                        .and_then(NopLayer.with_filter(filter_fn(|_| true)))
                        .with_filter(LevelFilter::WARN),
                ));
            assert_eq!(dbg!(subscriber).max_level_hint(), Some(LevelFilter::INFO));
        }

        #[test]
        fn plf_only_picks_max() {
            let subscriber = crate::registry()
                .with(NopLayer.with_filter(LevelFilter::WARN))
                .with(NopLayer.with_filter(LevelFilter::DEBUG));
            assert_eq!(dbg!(subscriber).max_level_hint(), Some(LevelFilter::DEBUG));
        }

        #[test]
        fn many_plf_only_picks_max() {
            let subscriber = crate::registry()
                .with(NopLayer.with_filter(LevelFilter::WARN))
                .with(NopLayer.with_filter(LevelFilter::DEBUG))
                .with(NopLayer.with_filter(LevelFilter::INFO))
                .with(NopLayer.with_filter(LevelFilter::ERROR));
            assert_eq!(dbg!(subscriber).max_level_hint(), Some(LevelFilter::DEBUG));
        }

        #[test]
        fn nested_plf_only_picks_max() {
            let subscriber = crate::registry()
                .with(
                    NopLayer.with_filter(LevelFilter::INFO).and_then(
                        NopLayer
                            .with_filter(LevelFilter::WARN)
                            .and_then(NopLayer.with_filter(LevelFilter::DEBUG)),
                    ),
                )
                .with(
                    NopLayer
                        .with_filter(LevelFilter::INFO)
                        .and_then(NopLayer.with_filter(LevelFilter::ERROR)),
                );
            assert_eq!(dbg!(subscriber).max_level_hint(), Some(LevelFilter::DEBUG));
        }
    }
}
