# TODO — LAN multi-operator networking

The build plan for **LAN sharing of contacts, heard-stations, and working-intent**
between operators (the third DM420 differentiator). Spec: **`docs/networking.md`**
(authoritative for the protocol). This file is the cross-session task tracker.

**Shape of the work:** four incremental steps, each independently demoable on two
instances (a laptop + a Raspberry Pi on the bench is enough — full multi-op station
not required until Field Day). Settled decisions: mDNS (`mdns-sd`) discovery + plain
UDP gossip, no central node, eventual consistency; share worked + heard + intent;
snapshots are latest-wins State, the log G-set converges via an anti-entropy loop
(per-origin `seq`-range digests → `LogRequest` → MTU-chunked `LogReply`).

## Decisions to settle before Step 2

These shape the wire format, so lock them in first (see `docs/networking.md` →
"Open questions"):

- [ ] **Wire encoding** — `serde_json` (current, debuggable) vs. `bincode` (compact).
  Plan: stay JSON through Step 2 bring-up, switch once the schema settles.
- [ ] **Per-station log `seq` persistence** — persist the high-water mark next to
  `logbook.json` so a restarted op doesn't reissue ids a peer already merged, vs.
  rely on `(origin, seq)` global uniqueness. Recommend persisting.
- [ ] **Security posture** — LAN-trust-only for v1 (no auth/encryption). Confirm
  that's acceptable for the Field Day club network.

---

## Step 1 — transport + discovery + beacon  ✅ DONE

The `net` crate skeleton: instances find each other and exchange (empty) snapshots.

- [x] `§9` types in `crates/types/src/lib.rs`: `WorkingTarget`, `HeardStation`,
  `StationSnapshot` (filled the old DEFERRED placeholder).
- [x] Register `StationSnapshot` as `State` in `crates/bus/src/message.rs`.
- [x] `net` crate: `wire.rs` (`Frame`/`Wire` enum + range types + JSON codec),
  `peers.rs` (target set + per-station `seq` dedup/TTL), `discovery.rs` (mDNS
  announce + browse), `lib.rs` (UDP socket, beacon loop, receive loop → re-publish
  peers' snapshots onto `station/{id}/snapshot`).
- [x] Wire into `core::spawn` behind `DM420_NET` (env: `DM420_STATION_ID`,
  `DM420_NET_PORT`, `DM420_PEERS`); best-effort, degrades to a warning.
- [x] Unit tests (wire round-trip, peer dedup/expiry); loopback UDP path verified.
- [ ] **Two-host smoke test (Josh):** run on laptop + Pi, confirm `net: snapshot
  from peer` in the logs (or cross `DM420_PEERS` in two terminals if mDNS blocked).

---

## Step 2 — shared logbook (the headline win)

Gossip completed contacts so every operator's logbook converges. **Most useful
first result on two boxes.**

- [ ] **Outbound push.** In `net`, subscribe `logbook/entries`; for each entry with
  `origin == my StationId`, buffer into the G-set and `LogPush` to peer targets.
  **Push guard:** never proactively re-push `origin != me` entries (echo-storm).
- [ ] **Inbound merge.** On `LogPush`/`LogReply`, for each `QsoId` not held, add to
  the G-set and **publish onto `logbook/entries`** — the existing logbook service
  (`crates/logbook/src/lib.rs`) is already subscribed there and persists it with no
  new code. Idempotent (drop already-held ids).
- [ ] **Anti-entropy loop.** `DIGEST_INTERVAL` (~15 s, jittered) broadcast
  `LogDigest(Vec<OriginHave>)` = our holdings as per-origin `seq` ranges. On a
  peer's digest, diff against our holdings and `LogRequest` the gap; serve
  `LogRequest` ranges (any origin — pull-based relay) as MTU-chunked `LogReply`s.
- [ ] **G-set + range math.** A holdings structure keyed by `StationId` →
  sorted `seq` ranges; helpers to insert a `seq`, diff two range sets, and split a
  served range across MTU-sized batches. Unit-test the range merge/diff thoroughly
  (this is the correctness core — covers the "offline for an hour" convergence).
- [ ] **`seq` persistence** (if decided above): high-water mark file alongside the
  logbook; seed `qso::shell`'s sequence from it instead of the wall clock.
- [ ] **UI: origin distinction.** Add `origin: StationId` to `MapSpot`
  (`crates/gui/src/bus_view.rs`) and the logbook rows; visually distinguish mine
  vs. peer in the **logbook** and **map/contacts** panels (per `docs/log_book.md`,
  `docs/map_panel.md` — *heard ≠ worked, mine ≠ peer*). `worked_spots()` currently
  pulls all entries equally and ignores origin.
- [ ] **Acceptance:** two instances, A logs 100+ while B is offline; on B's rejoin
  the logs fully converge; B's panels show A's contacts marked as peer-origin. Lossy
  network (drop datagrams) still converges within a few digest rounds.

---

## Step 3 — working-intent ("don't compete")

Publish what you're working so peers don't double up; consume theirs.

- [ ] **Emit intent.** In `net`, subscribe `qso/{id}/state`; map the engine phase to
  `WorkingTarget`: `Armed`/`Calling`/`InExchange` → `Some`, `Idle`/`Complete`/
  `TimedOut` → `None`. Attach to the periodic snapshot **and** push an immediate
  snapshot on change (collision avoidance is latency-sensitive).
- [ ] **Populate the snapshot.** Replace the Step-1 empty payload: real `working`
  (above) on each `StationSnapshot`.
- [ ] **Consume peers' intent.** Surface `station/*/snapshot` `working` in the GUI:
  flag a station another op is working in the **waterslide** (and the map crosshair,
  cf. `TODO.md`).
- [ ] **Auto-pick exclusion.** When the multi-caller auto-pick lands in the QSO
  engine (`crates/qso/src/engine.rs:14-16` notes this is the hook), exclude
  `WorkedByNetwork` and currently-`working` stations; manual number-key override
  still wins.
- [ ] **Acceptance:** op A arms to a station; within ~1 beacon op B sees it flagged
  and auto-pick skips it; when A drops to Idle the flag clears.

---

## Step 4 — heard-station + band-activity aggregation, then follow-ons

Surface what the *whole network* is hearing, not just this receiver.

- [ ] **Emit heard.** Subscribe `radio/{id}/decodes_enriched`; fold into a
  recency-bounded `heard: Vec<HeardStation>` on the snapshot (cap so it fits one
  datagram; drop oldest first).
- [ ] **Emit band activity.** Attach `band_activity: Vec<BandActivity>` (already a
  type) from the scanner/decode counts.
- [ ] **Consume.** Map + band-scan panels aggregate peers' heard/activity, aged by
  **local** receive time; distinguish my ears from peers'.
- [ ] **ADIF import/export** (logbook crate; `docs/log_book.md`, OVERVIEW §7) — the
  amateur-radio interchange format; pairs naturally with the merged log.
- [ ] **Multi-radio-in-one-box** (Field-Day-timed): spawn >1 radio id, a radio
  selector/added-radio config UI, and inter-radio PTT interlock. Bus topics already
  scope by `RadioId`; only `rig0` is instantiated today.

---

## Reference

- Spec / protocol: `docs/networking.md`
- Message vocabulary: `docs/message-catalog.md` §9, `crates/types/src/lib.rs` §9
- Bus topic scoping: `crates/bus/src/topic.rs` (`StationSnapshot(StationId)` →
  `station/{id}/snapshot`)
- Service-spawn pattern to mirror: `core::spawn`, `logbook::spawn`, `qso::shell::spawn`
- Env config (interim, move into `CoreConfig`/`Settings` later):
  `net::NetConfig::from_env`
