//! PTT interlock granter: owns the single TX token.
//!
//! This is the authority that `allow_transmit` unlocks. It enforces a **single
//! live holder** of the [`InterlockToken`] and a TTL, so a crashed or runaway TX
//! client cannot wedge the transmitter. A TX client (the QSO shell) acquires a
//! token over the bus (`interlock/{id}`, served by [`serve`]); the rig adapter
//! validates every keying `PttRequest` against the live grant **in process**
//! ([`Granter::validate`]) — no bus round-trip on the hot keying path.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bus::types as t;
use bus::{BusHandle, Topic};

/// How long a grant stays valid. Covers a full FT8 slot (~15 s) plus margin; the
/// rig's own 15 s PTT watchdog is the finer-grained stuck-key guard.
const GRANT_TTL: Duration = Duration::from_secs(20);

struct State {
    /// The live grant: token + when it expires. `None` ⇒ no holder.
    held: Option<(t::InterlockToken, Instant)>,
    /// Monotonic token source (never reused, so a stale token never validates).
    next: u64,
}

/// The PTT token authority. Cheap to clone (shared state); one per radio.
#[derive(Clone)]
pub struct Granter {
    state: Arc<Mutex<State>>,
    ttl: Duration,
}

impl Default for Granter {
    fn default() -> Self {
        Self::new(GRANT_TTL)
    }
}

impl Granter {
    pub fn new(ttl: Duration) -> Self {
        Self {
            state: Arc::new(Mutex::new(State { held: None, next: 1 })),
            ttl,
        }
    }

    /// Grant the token if no live holder exists. Single-holder: a second acquire
    /// while a grant is live is [`InterlockError::Denied`].
    pub fn acquire(&self) -> t::InterlockReply {
        let mut s = self.state.lock().unwrap();
        let now = Instant::now();
        // Reclaim an expired grant before deciding.
        if let Some((_, exp)) = s.held
            && now >= exp
        {
            s.held = None;
        }
        if s.held.is_some() {
            return t::InterlockReply::Denied(t::InterlockError::Denied);
        }
        let token = t::InterlockToken(s.next);
        s.next += 1;
        s.held = Some((token, now + self.ttl));
        t::InterlockReply::Granted {
            token,
            ttl_ms: self.ttl.as_millis() as u64,
        }
    }

    /// Release the token early (otherwise it lapses at TTL). Only the holder may
    /// release; anyone else gets [`InterlockError::NotHolder`].
    pub fn release(&self, token: t::InterlockToken) -> t::InterlockReply {
        let mut s = self.state.lock().unwrap();
        match s.held {
            Some((held, _)) if held == token => {
                s.held = None;
                t::InterlockReply::Released
            }
            _ => t::InterlockReply::Denied(t::InterlockError::NotHolder),
        }
    }

    /// Extend the current holder's grant by another TTL. Returns `true` if `token`
    /// is the live holder (its expiry was pushed out), `false` otherwise. Lets a
    /// long-running holder — the band scanner, which holds TX off for a whole
    /// multi-minute sweep — keep the token alive past the TTL **without** a
    /// release/re-acquire gap another client could slip a transmission through.
    pub fn refresh(&self, token: t::InterlockToken) -> bool {
        let mut s = self.state.lock().unwrap();
        match s.held {
            Some((held, _)) if held == token => {
                s.held = Some((held, Instant::now() + self.ttl));
                true
            }
            _ => false,
        }
    }

    /// Whether `token` is the current, unexpired holder — checked on every keying
    /// PttRequest by the rig adapter.
    pub fn validate(&self, token: t::InterlockToken) -> bool {
        let s = self.state.lock().unwrap();
        match s.held {
            Some((held, exp)) => held == token && Instant::now() < exp,
            None => false,
        }
    }
}

/// Serve `interlock/{id}` so bus clients (the QSO shell) can acquire/release the
/// token. Spawns onto the current tokio runtime, like the other `core` servers.
pub fn serve(bus: &BusHandle, radio: t::RadioId, granter: Granter) {
    let mut server = match bus
        .serve::<t::InterlockRequest, t::InterlockReply>(&Topic::Interlock(radio.clone()))
    {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("interlock: cannot serve for {radio:?}: {e:?}");
            return;
        }
    };
    tokio::spawn(async move {
        while let Some((req, responder)) = server.next().await {
            let reply = match req {
                t::InterlockRequest::Acquire => granter.acquire(),
                t::InterlockRequest::Release(token) => granter.release(token),
            };
            match &reply {
                t::InterlockReply::Granted { token, .. } => {
                    tracing::debug!(?token, "interlock: token granted")
                }
                t::InterlockReply::Released => tracing::debug!("interlock: token released"),
                t::InterlockReply::Denied(d) => tracing::debug!(reason = ?d, "interlock: denied"),
            }
            responder.reply(reply);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(reply: t::InterlockReply) -> t::InterlockToken {
        match reply {
            t::InterlockReply::Granted { token, .. } => token,
            other => panic!("expected Granted, got {other:?}"),
        }
    }

    #[test]
    fn single_holder_until_released() {
        let g = Granter::new(Duration::from_secs(60));
        let a = token(g.acquire());
        assert!(g.validate(a));
        // A second acquire is denied while the first grant is live.
        assert!(matches!(
            g.acquire(),
            t::InterlockReply::Denied(t::InterlockError::Denied)
        ));
        // A non-holder cannot release.
        assert!(matches!(
            g.release(t::InterlockToken(9999)),
            t::InterlockReply::Denied(t::InterlockError::NotHolder)
        ));
        // The holder releases; the token stops validating and a fresh, distinct
        // token can be acquired.
        assert!(matches!(g.release(a), t::InterlockReply::Released));
        assert!(!g.validate(a));
        let b = token(g.acquire());
        assert_ne!(a, b);
    }

    #[test]
    fn grant_expires_and_frees_the_holder() {
        let g = Granter::new(Duration::from_millis(0)); // grant is born expired
        let a = token(g.acquire());
        // Already past its TTL: does not validate, and the next acquire succeeds.
        assert!(!g.validate(a));
        assert!(matches!(g.acquire(), t::InterlockReply::Granted { .. }));
    }

    #[test]
    fn refresh_extends_only_for_the_holder() {
        let g = Granter::new(Duration::from_secs(60));
        let a = token(g.acquire());
        assert!(g.refresh(a)); // the holder can extend its grant
        assert!(!g.refresh(t::InterlockToken(9999))); // a non-holder cannot
        assert!(matches!(g.release(a), t::InterlockReply::Released));
        assert!(!g.refresh(a)); // nothing to refresh once released
    }
}
