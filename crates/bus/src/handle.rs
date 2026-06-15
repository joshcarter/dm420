//! `BusHandle` and its subscription/command handles — the in-process transport.
//!
//! ## How delivery is type-erased
//!
//! The hot path never serializes. Each topic owns a concrete typed channel
//! (`watch` / `broadcast` / per-subscriber `mpsc`) stored in a registry as
//! `Box<dyn Any + Send + Sync>`; publish/subscribe downcast back to the concrete
//! payload type. The payload type for a topic is fixed by its first user; a later
//! caller with a different type for the same (class-matching) topic gets
//! [`BusError::ClassMismatch`] on the failed downcast.
//!
//! ## Wildcard routing
//!
//! A `Wildcard(kind)` subscription is served by a parallel per-kind entry. Every
//! `publish` delivers to the exact-topic channel *and*, if any wildcard
//! subscribers exist for that kind, to the kind-level channel. State wildcard
//! delivery is a live stream of updates (`broadcast`) with no snapshot, since a
//! single latest-value cell can't represent many scope ids at once.
//!
//! The recorder tap (record every published envelope) is layered on in
//! `recorder.rs`; this module is the live-delivery core.

use std::any::Any;
use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::{broadcast, mpsc, oneshot, watch};
use types::Timestamp;

use crate::error::BusError;
use crate::message::BusMessage;
use crate::recorder::{ENVELOPE_VERSION, Envelope};
use crate::topic::{DeliveryClass, Topic, TopicKind, TopicSelector};

/// Capacity of a `StreamLossy` broadcast channel (per topic and per wildcard kind).
const LOSSY_CAP: usize = 256;
/// Capacity of a `StreamLossless` per-subscriber mpsc queue. A subscriber that
/// lets this fill is dropped (it's a broken subscriber), never blocking publish.
const LOSSLESS_SUB_CAP: usize = 256;

/// Retained replay-ring size for a `StreamLossless` topic, by kind.
fn ring_capacity(kind: TopicKind) -> usize {
    match kind {
        TopicKind::Decodes | TopicKind::DecodesEnriched => 16,
        TopicKind::LogbookEntries => 4096,
        _ => 64,
    }
}

/// The reply channel handed alongside a command request.
type CmdMsg<Req, Rep> = (Req, oneshot::Sender<Rep>);

/// Per-topic fan-out state for a `StreamLossless` channel: live per-subscriber
/// queues plus a retained ring for late-join replay.
struct LosslessInner<M> {
    subs: Vec<mpsc::Sender<M>>,
    ring: VecDeque<M>,
    ring_cap: usize,
}

impl<M> LosslessInner<M> {
    fn new(ring_cap: usize) -> Self {
        Self {
            subs: Vec::new(),
            ring: VecDeque::new(),
            ring_cap,
        }
    }
}

impl<M: Clone> LosslessInner<M> {
    /// Retain `msg` in the ring and fan it out. A subscriber whose queue is full
    /// (or whose receiver is gone) is dropped here — its next `recv` returns
    /// `Closed`. The publisher never blocks.
    fn push(&mut self, msg: M) {
        if self.ring_cap > 0 {
            if self.ring.len() >= self.ring_cap {
                self.ring.pop_front();
            }
            self.ring.push_back(msg.clone());
        }
        self.subs
            .retain(|tx| matches!(tx.try_send(msg.clone()), Ok(())));
    }
}

/// One registry slot. The boxed value is the concrete typed channel for the
/// slot's delivery class.
enum Entry {
    /// `watch::Sender<Option<M>>`
    State(Box<dyn Any + Send + Sync>),
    /// `broadcast::Sender<M>`
    Lossy(Box<dyn Any + Send + Sync>),
    /// `Arc<Mutex<LosslessInner<M>>>`
    Lossless(Box<dyn Any + Send + Sync>),
    /// `mpsc::UnboundedSender<CmdMsg<Req, Rep>>`
    Command(Box<dyn Any + Send + Sync>),
}

/// Lazily-populated channel registry: exact topics by canonical string, plus a
/// parallel per-kind map for wildcard subscribers.
#[derive(Default)]
struct Registry {
    exact: HashMap<String, Entry>,
    wild: HashMap<TopicKind, Entry>,
}

struct Inner {
    reg: Mutex<Registry>,
    correlation: AtomicU64,
    recorder: Mutex<Option<RecorderSink>>,
}

/// The attached recorder's write end. Dropping it closes the writer task's queue,
/// which flushes and exits.
struct RecorderSink {
    tx: mpsc::UnboundedSender<Envelope>,
    version: u16,
}

/// Milliseconds since the Unix epoch, for stamping recorded envelopes.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// A cloneable handle to the in-process bus. Cheap to clone (`Arc` inside) and
/// usable from any number of tasks.
#[derive(Clone)]
pub struct BusHandle {
    inner: Arc<Inner>,
}

impl Default for BusHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl BusHandle {
    /// Create a fresh, empty bus.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                reg: Mutex::new(Registry::default()),
                correlation: AtomicU64::new(0),
                recorder: Mutex::new(None),
            }),
        }
    }

    // -----------------------------------------------------------------
    // publish
    // -----------------------------------------------------------------

    /// Publish to a `State` / `StreamLossy` / `StreamLossless` topic. Returns
    /// immediately and never blocks on a subscriber. Asserts
    /// `M::CLASS == topic.delivery_class()`.
    pub fn publish<M: BusMessage>(&self, topic: &Topic, msg: M) -> Result<(), BusError> {
        if M::CLASS != topic.delivery_class() {
            return Err(BusError::ClassMismatch);
        }
        // Commands flow through request/serve, not publish.
        if M::CLASS == DeliveryClass::Command {
            return Err(BusError::ClassMismatch);
        }
        let key = topic.canonical();
        let kind = topic.kind();
        self.record(&key, &msg); // recorder tap (no-op unless a recorder is attached)
        match M::CLASS {
            DeliveryClass::State => self.publish_state(&key, kind, msg),
            DeliveryClass::StreamLossy => self.publish_lossy(&key, kind, msg),
            DeliveryClass::StreamLossless => self.publish_lossless(&key, kind, msg),
            DeliveryClass::Command => unreachable!("command class rejected above"),
        }
    }

    // -----------------------------------------------------------------
    // recorder
    // -----------------------------------------------------------------

    /// Serialize one published message to an [`Envelope`] and hand it to the
    /// writer task. No-op (and no serialization cost) unless a recorder is
    /// attached.
    fn record<M: BusMessage>(&self, topic_key: &str, msg: &M) {
        let guard = self.inner.recorder.lock().unwrap();
        let Some(sink) = guard.as_ref() else {
            return;
        };
        if let Ok(payload) = serde_json::to_value(msg) {
            let env = Envelope {
                version: sink.version,
                topic: topic_key.to_string(),
                correlation: None,
                payload,
                recorded_at: Timestamp(now_ms()),
            };
            let _ = sink.tx.send(env);
        }
    }

    /// Tap every published message and write NDJSON envelopes to `path`. Opt-in:
    /// serialization cost is paid only while the returned [`RecorderHandle`] (or a
    /// clone of this bus's recorder slot) is attached. Replaces any prior recorder.
    pub fn attach_recorder(&self, path: &Path) -> Result<RecorderHandle, BusError> {
        let file = std::fs::File::create(path).map_err(|e| BusError::Serialization(e.to_string()))?;
        let (tx, mut rx) = mpsc::unbounded_channel::<Envelope>();
        let join = tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            let mut w = tokio::io::BufWriter::new(tokio::fs::File::from_std(file));
            while let Some(env) = rx.recv().await {
                if let Ok(mut line) = serde_json::to_string(&env) {
                    line.push('\n');
                    let _ = w.write_all(line.as_bytes()).await;
                }
            }
            let _ = w.flush().await;
        });
        *self.inner.recorder.lock().unwrap() = Some(RecorderSink {
            tx,
            version: ENVELOPE_VERSION,
        });
        Ok(RecorderHandle {
            join,
            bus: self.clone(),
        })
    }

    fn publish_state<M: BusMessage>(
        &self,
        key: &str,
        kind: TopicKind,
        msg: M,
    ) -> Result<(), BusError> {
        let mut guard = self.inner.reg.lock().unwrap();
        let reg = &mut *guard;

        let entry = reg.exact.entry(key.to_string()).or_insert_with(|| {
            let (tx, _rx) = watch::channel::<Option<M>>(None);
            Entry::State(Box::new(tx))
        });
        let Entry::State(b) = entry else {
            return Err(BusError::ClassMismatch);
        };
        let tx = b
            .downcast_ref::<watch::Sender<Option<M>>>()
            .ok_or(BusError::ClassMismatch)?;
        // `send_replace`, not `send`: `watch::Sender::send` is a no-op that drops
        // the value when `receiver_count() == 0`, which would break State late-join
        // (publish-before-any-subscriber). `send_replace` always stores the latest
        // value and still notifies any current receivers.
        tx.send_replace(Some(msg.clone()));

        if let Some(Entry::Lossy(b)) = reg.wild.get(&kind)
            && let Some(s) = b.downcast_ref::<broadcast::Sender<M>>()
        {
            let _ = s.send(msg);
        }
        Ok(())
    }

    fn publish_lossy<M: BusMessage>(
        &self,
        key: &str,
        kind: TopicKind,
        msg: M,
    ) -> Result<(), BusError> {
        let mut guard = self.inner.reg.lock().unwrap();
        let reg = &mut *guard;

        let entry = reg.exact.entry(key.to_string()).or_insert_with(|| {
            let (tx, _rx) = broadcast::channel::<M>(LOSSY_CAP);
            Entry::Lossy(Box::new(tx))
        });
        let Entry::Lossy(b) = entry else {
            return Err(BusError::ClassMismatch);
        };
        let tx = b
            .downcast_ref::<broadcast::Sender<M>>()
            .ok_or(BusError::ClassMismatch)?;
        let _ = tx.send(msg.clone());

        if let Some(Entry::Lossy(b)) = reg.wild.get(&kind)
            && let Some(s) = b.downcast_ref::<broadcast::Sender<M>>()
        {
            let _ = s.send(msg);
        }
        Ok(())
    }

    fn publish_lossless<M: BusMessage>(
        &self,
        key: &str,
        kind: TopicKind,
        msg: M,
    ) -> Result<(), BusError> {
        let (exact, wild) = {
            let mut guard = self.inner.reg.lock().unwrap();
            let reg = &mut *guard;
            let exact = goc_lossless::<M, String>(&mut reg.exact, key.to_string(), ring_capacity(kind))?;
            let wild = match reg.wild.get(&kind) {
                Some(Entry::Lossless(b)) => b
                    .downcast_ref::<Arc<Mutex<LosslessInner<M>>>>()
                    .cloned(),
                _ => None,
            };
            (exact, wild)
        };
        exact.lock().unwrap().push(msg.clone());
        if let Some(w) = wild {
            w.lock().unwrap().push(msg);
        }
        Ok(())
    }

    // -----------------------------------------------------------------
    // subscribe
    // -----------------------------------------------------------------

    /// Subscribe to a topic or a whole topic kind. Snapshot semantics on the
    /// first `recv` (per class): a `State` sub yields the current value (if any),
    /// a `StreamLossless` sub replays the retained ring in order, a `StreamLossy`
    /// sub yields only future messages.
    pub fn subscribe<M: BusMessage>(
        &self,
        sel: TopicSelector,
    ) -> Result<Subscription<M>, BusError> {
        let (key, kind, class) = match &sel {
            TopicSelector::Exact(t) => {
                if M::CLASS != t.delivery_class() {
                    return Err(BusError::ClassMismatch);
                }
                (Some(t.canonical()), t.kind(), t.delivery_class())
            }
            TopicSelector::Wildcard(k) => {
                if M::CLASS != k.delivery_class() {
                    return Err(BusError::ClassMismatch);
                }
                (None, *k, k.delivery_class())
            }
        };

        let inner = match (key, class) {
            // ---- exact ----
            (Some(key), DeliveryClass::State) => {
                let mut guard = self.inner.reg.lock().unwrap();
                let entry = guard.exact.entry(key).or_insert_with(|| {
                    let (tx, _rx) = watch::channel::<Option<M>>(None);
                    Entry::State(Box::new(tx))
                });
                let Entry::State(b) = entry else {
                    return Err(BusError::ClassMismatch);
                };
                let rx = b
                    .downcast_ref::<watch::Sender<Option<M>>>()
                    .ok_or(BusError::ClassMismatch)?
                    .subscribe();
                SubInner::State { rx, primed: false }
            }
            (Some(key), DeliveryClass::StreamLossy) => {
                let mut guard = self.inner.reg.lock().unwrap();
                let entry = guard.exact.entry(key).or_insert_with(|| {
                    let (tx, _rx) = broadcast::channel::<M>(LOSSY_CAP);
                    Entry::Lossy(Box::new(tx))
                });
                let Entry::Lossy(b) = entry else {
                    return Err(BusError::ClassMismatch);
                };
                let rx = b
                    .downcast_ref::<broadcast::Sender<M>>()
                    .ok_or(BusError::ClassMismatch)?
                    .subscribe();
                SubInner::Stream { rx }
            }
            (Some(key), DeliveryClass::StreamLossless) => {
                let arc = {
                    let mut guard = self.inner.reg.lock().unwrap();
                    goc_lossless::<M, String>(&mut guard.exact, key, ring_capacity(kind))?
                };
                lossless_subscription(&arc)
            }
            // ---- wildcard ----
            (None, DeliveryClass::State) | (None, DeliveryClass::StreamLossy) => {
                // Both served by a per-kind broadcast (State wildcard = live updates).
                let mut guard = self.inner.reg.lock().unwrap();
                let entry = guard.wild.entry(kind).or_insert_with(|| {
                    let (tx, _rx) = broadcast::channel::<M>(LOSSY_CAP);
                    Entry::Lossy(Box::new(tx))
                });
                let Entry::Lossy(b) = entry else {
                    return Err(BusError::ClassMismatch);
                };
                let rx = b
                    .downcast_ref::<broadcast::Sender<M>>()
                    .ok_or(BusError::ClassMismatch)?
                    .subscribe();
                SubInner::Stream { rx }
            }
            (None, DeliveryClass::StreamLossless) => {
                let arc = {
                    let mut guard = self.inner.reg.lock().unwrap();
                    goc_lossless::<M, TopicKind>(&mut guard.wild, kind, ring_capacity(kind))?
                };
                lossless_subscription(&arc)
            }
            // Commands are not subscribable.
            (_, DeliveryClass::Command) => return Err(BusError::ClassMismatch),
        };
        Ok(Subscription { inner })
    }

    // -----------------------------------------------------------------
    // request / serve  (Command topics)
    // -----------------------------------------------------------------

    /// Register the single server for a Command topic. `ServerExists` if a server
    /// is already registered for it.
    pub fn serve<Req, Rep>(&self, topic: &Topic) -> Result<RequestStream<Req, Rep>, BusError>
    where
        Req: BusMessage,
        Rep: BusMessage,
    {
        if topic.delivery_class() != DeliveryClass::Command || Req::CLASS != DeliveryClass::Command {
            return Err(BusError::ClassMismatch);
        }
        let key = topic.canonical();
        let mut guard = self.inner.reg.lock().unwrap();
        if guard.exact.contains_key(&key) {
            return Err(BusError::ServerExists);
        }
        let (tx, rx) = mpsc::unbounded_channel::<CmdMsg<Req, Rep>>();
        guard.exact.insert(key, Entry::Command(Box::new(tx)));
        Ok(RequestStream { rx })
    }

    /// Send a request to a Command topic and await the reply (or time out).
    /// `NoHandler` if no server is registered; `Timeout` if no reply in time.
    pub async fn request<Req, Rep>(
        &self,
        topic: &Topic,
        req: Req,
        timeout: Duration,
    ) -> Result<Rep, BusError>
    where
        Req: BusMessage,
        Rep: BusMessage,
    {
        if topic.delivery_class() != DeliveryClass::Command || Req::CLASS != DeliveryClass::Command {
            return Err(BusError::ClassMismatch);
        }
        // Minted for the recorder/envelope and future network parity; in-process
        // routing uses the oneshot reply channel directly.
        let _correlation = self.inner.correlation.fetch_add(1, Ordering::Relaxed);

        let key = topic.canonical();
        let tx = {
            let guard = self.inner.reg.lock().unwrap();
            match guard.exact.get(&key) {
                Some(Entry::Command(b)) => b
                    .downcast_ref::<mpsc::UnboundedSender<CmdMsg<Req, Rep>>>()
                    .cloned()
                    .ok_or(BusError::ClassMismatch)?,
                Some(_) => return Err(BusError::ClassMismatch),
                None => return Err(BusError::NoHandler),
            }
        };

        let (reply_tx, reply_rx) = oneshot::channel::<Rep>();
        tx.send((req, reply_tx)).map_err(|_| BusError::NoHandler)?;
        match tokio::time::timeout(timeout, reply_rx).await {
            Ok(Ok(rep)) => Ok(rep),
            Ok(Err(_)) => Err(BusError::Closed), // responder dropped without replying
            Err(_) => Err(BusError::Timeout),
        }
    }
}

/// Get-or-create the `LosslessInner` arc for a registry slot, generic over the
/// key type so it serves both the exact (`String`) and wildcard (`TopicKind`) maps.
fn goc_lossless<M: BusMessage, K: Eq + Hash>(
    map: &mut HashMap<K, Entry>,
    key: K,
    ring_cap: usize,
) -> Result<Arc<Mutex<LosslessInner<M>>>, BusError> {
    let entry = map.entry(key).or_insert_with(|| {
        Entry::Lossless(Box::new(Arc::new(Mutex::new(LosslessInner::<M>::new(ring_cap)))))
    });
    let Entry::Lossless(b) = entry else {
        return Err(BusError::ClassMismatch);
    };
    b.downcast_ref::<Arc<Mutex<LosslessInner<M>>>>()
        .cloned()
        .ok_or(BusError::ClassMismatch)
}

/// Register a new subscriber on a lossless channel: snapshot the current ring,
/// then attach a fresh per-subscriber queue for live delivery.
fn lossless_subscription<M: BusMessage>(arc: &Arc<Mutex<LosslessInner<M>>>) -> SubInner<M> {
    let mut g = arc.lock().unwrap();
    let snapshot = g.ring.clone();
    let (tx, rx) = mpsc::channel::<M>(LOSSLESS_SUB_CAP);
    g.subs.push(tx);
    SubInner::Lossless { snapshot, rx }
}

/// A live subscription. `recv` yields snapshot item(s) first (per class), then
/// live messages.
pub struct Subscription<M> {
    inner: SubInner<M>,
}

enum SubInner<M> {
    State {
        rx: watch::Receiver<Option<M>>,
        primed: bool,
    },
    Stream {
        rx: broadcast::Receiver<M>,
    },
    Lossless {
        snapshot: VecDeque<M>,
        rx: mpsc::Receiver<M>,
    },
}

impl<M: BusMessage> Subscription<M> {
    /// Receive the next message. `Err(Lagged)` if a lossy subscriber fell behind
    /// (still live — keep reading); `Err(Closed)` if a lossless subscription was
    /// dropped for being too slow (re-subscribe for a fresh snapshot) or the
    /// channel is otherwise gone.
    pub async fn recv(&mut self) -> Result<M, BusError> {
        match &mut self.inner {
            SubInner::State { rx, primed } => {
                if !*primed {
                    *primed = true;
                    if let Some(v) = rx.borrow_and_update().clone() {
                        return Ok(v);
                    }
                }
                loop {
                    rx.changed().await.map_err(|_| BusError::Closed)?;
                    if let Some(v) = rx.borrow_and_update().clone() {
                        return Ok(v);
                    }
                }
            }
            SubInner::Stream { rx } => match rx.recv().await {
                Ok(v) => Ok(v),
                Err(broadcast::error::RecvError::Lagged(n)) => Err(BusError::Lagged { skipped: n }),
                Err(broadcast::error::RecvError::Closed) => Err(BusError::Closed),
            },
            SubInner::Lossless { snapshot, rx } => {
                if let Some(v) = snapshot.pop_front() {
                    return Ok(v);
                }
                match rx.recv().await {
                    Some(v) => Ok(v),
                    None => Err(BusError::Closed),
                }
            }
        }
    }
}

/// The server side of a Command topic: a stream of `(request, responder)` pairs.
pub struct RequestStream<Req, Rep> {
    rx: mpsc::UnboundedReceiver<CmdMsg<Req, Rep>>,
}

impl<Req: BusMessage, Rep: BusMessage> RequestStream<Req, Rep> {
    /// Await the next request. `None` when all requesters are gone.
    pub async fn next(&mut self) -> Option<(Req, Responder<Rep>)> {
        let (req, reply) = self.rx.recv().await?;
        Some((req, Responder { reply }))
    }
}

/// Holds the reply route for one request. Reply exactly once.
pub struct Responder<Rep> {
    reply: oneshot::Sender<Rep>,
}

impl<Rep> Responder<Rep> {
    /// Send the reply. A dropped `Responder` surfaces as `Closed` at the requester.
    pub fn reply(self, rep: Rep) {
        let _ = self.reply.send(rep);
    }
}

/// Handle to an attached recorder. Drop it to leave the recorder running, or call
/// [`RecorderHandle::stop`] to detach, flush, and wait for the writer to finish.
pub struct RecorderHandle {
    join: tokio::task::JoinHandle<()>,
    bus: BusHandle,
}

impl RecorderHandle {
    /// Detach the recorder (so the publish tap stops serializing), flush all
    /// buffered envelopes to disk, and wait for the writer task to exit. Await
    /// this before reading the recorded file back (e.g. in a record→replay test).
    pub async fn stop(self) {
        *self.bus.inner.recorder.lock().unwrap() = None;
        let _ = self.join.await;
    }
}
