//! Minimal request/serve demo: a fake rig manager serves its command topic and
//! replies; a client sends a `SetFreq` and awaits the result. Run with:
//!
//! ```text
//! cargo run -p bus --example fake_command
//! ```

use std::time::Duration;

use serde::{Deserialize, Serialize};

use bus::types::*;
use bus::{BusError, BusHandle, BusMessage, DeliveryClass, Topic};

/// A command reply type. The orphan rule allows `impl BusMessage` here because the
/// type is local to this example crate. Command reply types are chosen per call
/// site, so this lives with the caller rather than in the shared `types` crate.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
enum CommandResult {
    Ok,
    Err(String),
}
impl BusMessage for CommandResult {
    const CLASS: DeliveryClass = DeliveryClass::Command;
}

#[tokio::main]
async fn main() -> Result<(), BusError> {
    let bus = BusHandle::new();
    let id = RadioId("k1".into());
    let topic = Topic::RigCommand(id.clone());

    // Fake rig manager: serve the command topic, apply, reply.
    let mut server = bus.serve::<RigCommand, CommandResult>(&topic)?;
    tokio::spawn(async move {
        while let Some((cmd, responder)) = server.next().await {
            println!("rig received: {cmd:?}");
            // (real impl would drive CAT here)
            responder.reply(CommandResult::Ok);
        }
    });

    // Client: tune the radio and await the outcome.
    let reply = bus
        .request::<RigCommand, CommandResult>(
            &topic,
            RigCommand::SetFreq(AbsHz(14_074_000)),
            Duration::from_secs(1),
        )
        .await?;
    println!("reply: {reply:?}");
    Ok(())
}
