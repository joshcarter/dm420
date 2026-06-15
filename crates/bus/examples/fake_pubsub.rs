//! Minimal publish/subscribe demo against the bus with no radio attached.
//!
//! A fake producer publishes a `RigState` (latest-wins) and a few `Decode`s
//! (lossless); a consumer subscribes to *all* radios' decodes via a wildcard and
//! prints each CQ. Run with:
//!
//! ```text
//! cargo run -p bus --example fake_pubsub
//! ```

use std::time::Duration;

use bus::types::*;
use bus::{BusHandle, Topic, TopicKind, TopicSelector};

#[tokio::main]
async fn main() {
    let bus = BusHandle::new();
    let id = RadioId("k1".into());

    // Consumer: every radio's decodes (wildcard by kind).
    let mut decodes = bus
        .subscribe::<Decode>(TopicSelector::Wildcard(TopicKind::Decodes))
        .expect("subscribe");
    let consumer = tokio::spawn(async move {
        let mut seen = 0;
        while let Ok(d) = decodes.recv().await {
            if let DecodeContent::Slotted {
                message: ParsedMessage::Cq { caller, grid, .. },
                ..
            } = &d.content
            {
                println!(
                    "decode: CQ {} ({})",
                    caller.0,
                    grid.as_ref().map(|g| g.0.as_str()).unwrap_or("?")
                );
            }
            seen += 1;
            if seen >= 3 {
                break;
            }
        }
    });

    // State: published once, readable by a late subscriber.
    bus.publish(
        &Topic::RigState(id.clone()),
        RigState {
            radio: id.clone(),
            vfo: AbsHz(14_074_000),
            rig_mode: RigMode::UsbData,
            ptt: false,
            meters: Meters::default(),
        },
    )
    .expect("publish rig state");

    // Producer: three CQ decodes.
    for call in ["N0JDC", "W4LL", "K1ABC"] {
        let d = Decode {
            radio: id.clone(),
            mode: OverAirMode::Ft8,
            t: Timestamp(0),
            offset: OffsetHz(1500.0),
            snr_db: Some(-10),
            source: SignalSource::Received,
            content: DecodeContent::Slotted {
                slot: SlotId(0),
                dt: 0.1,
                message: ParsedMessage::Cq {
                    caller: Callsign(call.into()),
                    contest: None,
                    grid: Some(GridSquare("DN70".into())),
                },
            },
        };
        bus.publish(&Topic::Decodes(id.clone()), d).expect("publish decode");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Late-join read of the State topic.
    let mut rig = bus
        .subscribe::<RigState>(TopicSelector::Exact(Topic::RigState(id.clone())))
        .expect("subscribe rig");
    let rs = rig.recv().await.expect("recv rig state");
    println!("rig state: vfo {} Hz", rs.vfo.0);

    consumer.await.expect("consumer task");
}
