use alloc::{
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};
use deku::prelude::*;

// https://www.improv-wifi.com/serial/

// Packet format ################################################33

#[derive(Debug, PartialEq, DekuRead, DekuWrite)]
#[deku(magic = b"\x0A")]
struct AlwaysTen {}

#[derive(Debug, PartialEq, DekuRead, DekuWrite)]
#[deku(magic = b"IMPROV\x01")]
pub struct ImprovWifiPacket {
    #[deku(writer = "Self::data_type_writer(deku::writer, data)")]
    data_type: u8,
    #[deku(writer = "Self::data_length_writer(deku::writer, data)")]
    data_length: u8,
    #[deku(ctx = "*data_type")]
    pub data: ImprovWifiPacketData,
    checksum: u8,
    always_ten: AlwaysTen,
}

impl ImprovWifiPacket {
    fn data_type_writer<W: no_std_io::io::Write>(
        writer: &mut deku::writer::Writer<W>,
        data: &ImprovWifiPacketData,
    ) -> Result<(), DekuError> {
        let value: u8 = data.deku_id().unwrap();
        value.to_writer(writer, deku::ctx::Endian::Big)
    }

    fn data_length_writer<W: no_std_io::io::Write>(
        writer: &mut deku::writer::Writer<W>,
        data: &ImprovWifiPacketData,
    ) -> Result<(), DekuError> {
        let value: u8 = data.get_data_length();
        value.to_writer(writer, deku::ctx::Endian::Big)
    }

    // builders

    pub fn new_current_state(current_state_option: CurrentStateOption) -> Self {
        ImprovWifiPacket {
            data_type: 0,
            data_length: 0,
            checksum: 0,
            always_ten: AlwaysTen {},
            data: ImprovWifiPacketData::CurrentState(current_state_option),
        }
    }

    pub fn new_error_state(error_state_option: ErrorStateOption) -> Self {
        ImprovWifiPacket {
            data_type: 0,
            data_length: 0,
            checksum: 0,
            always_ten: AlwaysTen {},
            data: ImprovWifiPacketData::ErrorState(error_state_option),
        }
    }
    pub fn new_rpc_result(rpc_result: RPCResultStruct) -> Self {
        ImprovWifiPacket {
            data_type: 0,
            data_length: 0,
            checksum: 0,
            always_ten: AlwaysTen {},
            data: ImprovWifiPacketData::RPCResult(rpc_result),
        }
    }

    pub fn new_rpc_command() -> Self {
        let rpc_command = RPCCommandStruct {
            command: 0x0,
            data_length: 0x0,
            data: RPCCommand::RequestCurrentState,
        };
        ImprovWifiPacket {
            checksum: 0,
            data_type: 0,
            data_length: 0,
            data: ImprovWifiPacketData::RPC(rpc_command),
            always_ten: AlwaysTen {},
        }
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, DekuError> {
        let mut bytes = <ImprovWifiPacket as DekuContainerWrite>::to_bytes(self)?;
        let checksum_pos = bytes.len() - 2;
        let checksum: u8 = bytes[..bytes.len() - 2]
            .iter()
            .fold(0, |acc, &x| acc.wrapping_add(x));
        bytes[checksum_pos] = checksum;
        Ok(bytes)
    }
}

#[derive(Debug, PartialEq, DekuRead, DekuWrite)]
#[deku(ctx = "data_type: u8", id = "data_type")]
pub enum ImprovWifiPacketData {
    #[deku(id = 0x01)]
    CurrentState(CurrentStateOption),
    #[deku(id = 0x02)]
    ErrorState(ErrorStateOption),

    #[deku(id = 0x03)]
    #[allow(clippy::upper_case_acronyms)]
    RPC(RPCCommandStruct),

    #[deku(id = 0x04)]
    #[allow(clippy::upper_case_acronyms)]
    RPCResult(RPCResultStruct),
}

impl ImprovWifiPacketData {
    pub fn get_data_length(&self) -> u8 {
        match self {
            ImprovWifiPacketData::CurrentState(current_state) => current_state.get_data_length(),
            ImprovWifiPacketData::ErrorState(error_state) => error_state.get_data_length(),
            ImprovWifiPacketData::RPC(rpc_command) => rpc_command.get_data_length(),
            ImprovWifiPacketData::RPCResult(rpc_result) => rpc_result.get_data_length(),
        }
    }
}

// Current State =================================

#[derive(Debug, PartialEq, DekuRead, DekuWrite)]
#[deku(id_type = "u8")]
pub enum CurrentStateOption {
    #[deku(id = 0x02)]
    Ready,
    #[deku(id = 0x03)]
    Provisioning,
    #[deku(id = 0x04)]
    Provisioned,
}

impl CurrentStateOption {
    pub fn get_data_length(&self) -> u8 {
        // always length 1
        0x01
    }
}

// Error State =================================

#[derive(Debug, PartialEq, DekuRead, DekuWrite)]
#[deku(id_type = "u8")]
pub enum ErrorStateOption {
    #[deku(id = 0x00)]
    NoError,
    #[deku(id = 0x01)]
    InvalidRPCPacket,
    #[deku(id = 0x02)]
    UnknownRPCCommand,
    #[deku(id = 0x03)]
    UnableToConnect,
    #[deku(id = 0xFF)]
    UnknownError,
}

impl ErrorStateOption {
    pub fn get_data_length(&self) -> u8 {
        // always length 1
        0x01
    }
}

// RPC Command ==============================

#[derive(Debug, PartialEq, DekuRead, DekuWrite)]
pub struct RPCCommandStruct {
    #[deku(writer = "Self::command_writer(deku::writer, data)")]
    command: u8,
    #[deku(writer = "Self::data_length_writer(deku::writer, data)")]
    data_length: u8,
    #[deku(ctx = "*command")]
    pub data: RPCCommand,
}

impl RPCCommandStruct {
    pub fn get_data_length(&self) -> u8 {
        2 + self.data.get_data_length()
    }

    fn command_writer<W: no_std_io::io::Write>(
        writer: &mut deku::writer::Writer<W>,
        data: &RPCCommand,
    ) -> Result<(), DekuError> {
        let value: u8 = data.deku_id().unwrap();
        value.to_writer(writer, deku::ctx::Endian::Big)
    }

    fn data_length_writer<W: no_std_io::io::Write>(
        writer: &mut deku::writer::Writer<W>,
        data: &RPCCommand,
    ) -> Result<(), DekuError> {
        let value: u8 = data.get_data_length();
        value.to_writer(writer, deku::ctx::Endian::Big)
    }
}

#[derive(Debug, PartialEq, DekuRead, DekuWrite)]
#[deku(ctx = "command: u8", id = "command")]
#[allow(clippy::enum_variant_names)]
pub enum RPCCommand {
    #[deku(id = 0x01)]
    SendWifiSettings(SendWifiSettingsStruct),
    #[deku(id = 0x02)]
    RequestCurrentState,
    #[deku(id = 0x03)]
    RequestDeviceInformation,
    #[deku(id = 0x04)]
    RequestScannedWifiNetworks,
}

impl RPCCommand {
    pub fn get_data_length(&self) -> u8 {
        // All commands are of data length 0, but writing explicitly
        match self {
            RPCCommand::SendWifiSettings(send_wifi_settings) => {
                send_wifi_settings.get_data_length()
            }
            RPCCommand::RequestCurrentState => 0x00,
            RPCCommand::RequestDeviceInformation => 0x00,
            RPCCommand::RequestScannedWifiNetworks => 0x00,
        }
    }
}

// Send Wi-Fi settings -------------------------------------------------

#[derive(Debug, PartialEq, DekuRead, DekuWrite)]
pub struct SendWifiSettingsStruct {
    pub ssid: DekuString,
    pub password: DekuString,
}

impl SendWifiSettingsStruct {
    fn get_data_length(&self) -> u8 {
        2 + self.ssid.content_len + self.password.content_len
    }
}

// RPC Result ==============================================

#[derive(Debug, PartialEq, DekuRead, DekuWrite)]
pub struct RPCResultStruct {
    command_responded: u8,
    #[deku(writer = "Self::string_data_length_writer(deku::writer, strings)")]
    strings_data_length: u8,
    #[deku(bytes_read = "strings_data_length")]
    strings: Vec<DekuString>,
}

impl RPCResultStruct {
    pub fn get_data_length(&self) -> u8 {
        let len: u8 = 1/*command_responded byte */+1 /*data_length byte*/ + Self::get_strings_data_length(&self.strings);
        len
    }
    fn string_data_length_writer<W: no_std_io::io::Write>(
        writer: &mut deku::writer::Writer<W>,
        data: &[DekuString],
    ) -> Result<(), DekuError> {
        let value: u8 = Self::get_strings_data_length(data);
        value.to_writer(writer, deku::ctx::Endian::Big)
    }
    fn get_strings_data_length(data: &[DekuString]) -> u8 {
        let value: u8 = data
            .iter()
            .fold(0, |acc, x| acc + 1/*string len byte*/ + x.content_len);
        value
    }

    // builders
    pub fn new_response_to_request_device_information(
        firmware_name: &str,
        firmware_version: &str,
        chip: &str,
        device_name: &str,
    ) -> Self {
        Self {
            command_responded: RPCCommand::RequestDeviceInformation.deku_id().unwrap(),
            strings_data_length: 0x00,
            strings: vec![
                firmware_name.into(),
                firmware_version.into(),
                chip.into(),
                device_name.into(),
            ],
        }
    }

    pub fn new_response_to_request_scanned_wifi_networks(
        ssid: &str,
        rssi: &str,
        auth_required: bool,
    ) -> Self {
        Self {
            command_responded: RPCCommand::RequestScannedWifiNetworks.deku_id().unwrap(),
            strings_data_length: 0x00,
            strings: vec![
                ssid.into(),
                rssi.into(),
                if auth_required {
                    "YES".into()
                } else {
                    "NO".into()
                },
            ],
        }
    }

    pub fn new_response_to_request_scanned_wifi_networks_end() -> Self {
        Self {
            command_responded: RPCCommand::RequestScannedWifiNetworks.deku_id().unwrap(),
            strings_data_length: 0x00,
            strings: vec![],
        }
    }

    pub fn new_response_to_send_wifi_settings(redirect_url: &str) -> Self {
        Self {
            command_responded: RPCCommand::SendWifiSettings(SendWifiSettingsStruct {
                ssid: DekuString::default(),
                password: DekuString::default(),
            })
            .deku_id()
            .unwrap(),
            strings_data_length: 0x00,
            strings: vec![redirect_url.into()],
        }
    }
}

// DekuString ========================================

#[derive(Debug, PartialEq, DekuRead, DekuWrite, Default)]
pub struct DekuString {
    content_len: u8,
    #[deku(count = "content_len")]
    content: Vec<u8>,
}
// impl DekuString {
//     pub fn new() -> Self {
//         Self {
//             content_len: 0,
//             content: Vec::new(),
//         }
//     }
// }
//
// impl Default for DekuString {
//     fn default() -> Self {
//         Self::new()
//     }
// }

impl From<String> for DekuString {
    fn from(value: String) -> Self {
        DekuString {
            content_len: value.len().try_into().unwrap(),
            content: Vec::from(value),
        }
    }
}
impl From<&str> for DekuString {
    fn from(value: &str) -> Self {
        DekuString {
            content_len: value.len().try_into().unwrap(),
            content: Vec::from(value),
        }
    }
}
impl From<DekuString> for String {
    fn from(val: DekuString) -> Self {
        String::from_utf8_lossy(&val.content).to_string()
    }
}

impl<'a> From<&'a DekuString> for &'a str {
    fn from(val: &'a DekuString) -> Self {
        core::str::from_utf8(&val.content).unwrap()
    }
}
