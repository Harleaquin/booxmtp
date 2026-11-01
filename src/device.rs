//! Device discovery and device selection.
//!
//! MTP talks to exactly one USB device per session. Discovery and opening are separated so the UI
//! can show a picker: [`list_devices`] enumerates without claiming anything, [`open`] claims the
//! selected device.
//!
//! Selection is deliberately strict: with several devices attached, the program refuses to pick
//! one silently and asks for `--device <SERIAL>` instead. With exactly one device, that device is
//! used. A serial number is matched exactly first, then ASCII case-insensitively.

use mtp_rs::UsbSpeed;
use mtp_rs::mtp::{MtpDevice, MtpDeviceInfo};

use crate::error::{Error, MtpResultExt, Result};

/// What the UI needs to know about a connected device before opening it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceSummary {
    /// USB serial number, when the device reports one. This is what `--device` matches.
    pub serial_number: Option<String>,
    /// Manufacturer string from the USB descriptor.
    pub manufacturer: Option<String>,
    /// Product/model string from the USB descriptor.
    pub model: Option<String>,
    /// USB topology location. Stable for a given port, useful for logs.
    pub location_id: u64,
    /// USB vendor ID.
    pub vendor_id: u16,
    /// USB product ID.
    pub product_id: u16,
    /// Negotiated link speed (device, cable and port), when the OS reports it.
    pub usb_speed: Option<UsbSpeed>,
}

impl DeviceSummary {
    /// Convert from the `mtp-rs` discovery record.
    pub fn from_info(info: &MtpDeviceInfo) -> Self {
        Self {
            serial_number: info.serial_number.clone(),
            manufacturer: info.manufacturer.clone(),
            model: info.product.clone(),
            location_id: info.location_id,
            vendor_id: info.vendor_id,
            product_id: info.product_id,
            usb_speed: info.speed,
        }
    }

    /// Human-readable model name, e.g. `BOOX Go Color 7`.
    pub fn display_name(&self) -> String {
        let manufacturer = self.manufacturer.as_deref().unwrap_or("Unknown vendor");
        let model = self.model.as_deref().unwrap_or("MTP device");
        format!("{manufacturer} {model}")
    }

    /// A single-line label for tables and error messages.
    pub fn label(&self) -> String {
        match &self.serial_number {
            Some(serial) => format!(
                "{} (serial: {serial}, location: {:08x})",
                self.display_name(),
                self.location_id
            ),
            None => format!(
                "{} (no serial, location: {:08x})",
                self.display_name(),
                self.location_id
            ),
        }
    }

    /// `VID:PID` in the usual `0x1234:0x5678` notation.
    pub fn vendor_product(&self) -> String {
        format!("{:04x}:{:04x}", self.vendor_id, self.product_id)
    }
}

/// Enumerate attached MTP devices without opening one.
///
/// Virtual devices registered by `mtp-rs`' test support show up here too, which is what the
/// integration tests rely on.
pub fn list_devices() -> Result<Vec<DeviceSummary>> {
    let devices = MtpDevice::list_devices().mtp_context("enumerating USB devices")?;
    let mut summaries: Vec<DeviceSummary> = devices.iter().map(DeviceSummary::from_info).collect();
    // Stable order regardless of USB enumeration order, so `devices` output is reproducible.
    summaries.sort_by(|left, right| {
        left.serial_number
            .cmp(&right.serial_number)
            .then_with(|| left.location_id.cmp(&right.location_id))
    });
    tracing::debug!(count = summaries.len(), "enumerated MTP devices");
    Ok(summaries)
}

/// Pick one device from a discovery result.
///
/// * no devices → [`Error::NoDevice`]
/// * one device and no selector → that device
/// * several devices and no selector → [`Error::MultipleDevices`] (never “the first one”)
/// * selector that matches nothing → [`Error::DeviceNotFound`]
pub fn select<'a>(
    devices: &'a [DeviceSummary],
    selector: Option<&str>,
) -> Result<&'a DeviceSummary> {
    if devices.is_empty() {
        return Err(Error::NoDevice);
    }

    let Some(wanted) = selector else {
        return match devices {
            [only] => Ok(only),
            _ => Err(Error::MultipleDevices {
                devices: devices.iter().map(DeviceSummary::label).collect(),
            }),
        };
    };

    let wanted = wanted.trim();
    devices
        .iter()
        .find(|device| device.serial_number.as_deref() == Some(wanted))
        .or_else(|| {
            devices.iter().find(|device| {
                device
                    .serial_number
                    .as_deref()
                    .is_some_and(|serial| serial.eq_ignore_ascii_case(wanted))
            })
        })
        .ok_or_else(|| Error::DeviceNotFound {
            requested: wanted.to_string(),
            available: devices.iter().map(DeviceSummary::label).collect(),
        })
}

/// Discover, select and open the device, returning the session handle and its description.
///
/// Opening is done by USB location (already known from discovery) rather than by re-enumerating,
/// so a device that is plugged into a different port mid-run fails loudly instead of silently
/// opening a different reader.
pub async fn open(selector: Option<&str>) -> Result<(MtpDevice, DeviceSummary)> {
    let devices = list_devices()?;
    let summary = select(&devices, selector)?.clone();
    tracing::debug!(device = %summary.label(), "opening MTP device");

    let device = MtpDevice::open_by_location(summary.location_id)
        .await
        .mtp_context(format!("opening {}", summary.label()))?;

    Ok((device, summary))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(serial: Option<&str>, location: u64) -> DeviceSummary {
        DeviceSummary {
            serial_number: serial.map(str::to_string),
            manufacturer: Some("BOOX".to_string()),
            model: Some("Go Color 7".to_string()),
            location_id: location,
            vendor_id: 0x1234,
            product_id: 0x5678,
            usb_speed: None,
        }
    }

    #[test]
    fn a_single_device_is_used_implicitly() {
        let devices = [device(Some("ABC"), 1)];
        assert_eq!(select(&devices, None).unwrap().location_id, 1);
    }

    #[test]
    fn several_devices_require_an_explicit_choice() {
        let devices = [device(Some("ABC"), 1), device(Some("DEF"), 2)];
        let error = select(&devices, None).unwrap_err();
        assert!(matches!(error, Error::MultipleDevices { .. }));
        assert_eq!(select(&devices, Some("DEF")).unwrap().location_id, 2);
    }

    #[test]
    fn serial_matching_is_case_insensitive_but_not_fuzzy() {
        let devices = [device(Some("AbC123"), 1)];
        assert!(select(&devices, Some("abc123")).is_ok());
        assert!(matches!(
            select(&devices, Some("abc")),
            Err(Error::DeviceNotFound { .. })
        ));
    }

    #[test]
    fn no_devices_is_reported_as_such() {
        assert!(matches!(select(&[], None), Err(Error::NoDevice)));
    }
}
