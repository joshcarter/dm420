//! Serial port discovery, with a hint at which port is likely the radio.

use serde::{Deserialize, Serialize};
use tracing::debug;

/// A discovered serial port plus a best-effort guess at whether it's a radio.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortInfo {
    pub name: String,
    pub description: Option<String>,
    pub vid: Option<u16>,
    pub pid: Option<u16>,
    pub product: Option<String>,
    /// USB device serial number (iSerial), when the OS exposes it. Unlike the
    /// device path (`/dev/cu.usbserial-{location}` on macOS, which is the USB
    /// location id and changes on every replug), this is stable across reconnects
    /// and ports — the durable key for "this is *my* radio".
    pub serial_number: Option<String>,
    /// True if this looks like a TS-590-class USB interface (Silicon Labs CP210x).
    pub likely_radio: bool,
}

/// List available serial ports. USB ports carry VID/PID/product/serial when the
/// OS exposes them; the Silicon Labs CP210x (VID 0x10C4) is flagged as a likely
/// TS-590.
pub fn list_ports() -> Result<Vec<PortInfo>, crate::RigError> {
    let ports = serialport::available_ports().map_err(crate::RigError::Serial)?;
    let mut out = Vec::with_capacity(ports.len());
    for p in ports {
        let mut info = PortInfo {
            name: p.port_name.clone(),
            description: None,
            vid: None,
            pid: None,
            product: None,
            serial_number: None,
            likely_radio: false,
        };
        if let serialport::SerialPortType::UsbPort(usb) = &p.port_type {
            info.vid = Some(usb.vid);
            info.pid = Some(usb.pid);
            info.product = usb.product.clone();
            info.serial_number = usb.serial_number.clone();
            // 0x10C4 = Silicon Labs (CP210x), as used by the TS-590 USB interface.
            info.likely_radio = usb.vid == 0x10C4;
        }
        debug!(name = %info.name, likely_radio = info.likely_radio, "found port");
        out.push(info);
    }
    Ok(out)
}
