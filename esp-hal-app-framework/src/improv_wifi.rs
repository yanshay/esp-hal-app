use alloc::{
    string::{String, ToString},
    vec,
    vec::Vec,
};

// Error type ================================================

#[derive(Debug, PartialEq)]
pub enum ParseError {
    Incomplete,
    InvalidMagic,
    InvalidChecksum,
    InvalidUtf8,
    InvalidDataType(u8),
    InvalidCommand(u8),
    InvalidState(u8),
    InvalidError(u8),
}

// Parser helper =============================================

struct Parser<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn read_u8(&mut self) -> Result<u8, ParseError> {
        if self.pos >= self.data.len() {
            return Err(ParseError::Incomplete);
        }
        let val = self.data[self.pos];
        self.pos += 1;
        Ok(val)
    }

    fn read_magic(&mut self, magic: &[u8]) -> Result<(), ParseError> {
        if self.pos + magic.len() > self.data.len() {
            return Err(ParseError::Incomplete);
        }
        if &self.data[self.pos..self.pos + magic.len()] != magic {
            return Err(ParseError::InvalidMagic);
        }
        self.pos += magic.len();
        Ok(())
    }

    fn read_vec(&mut self, count: usize) -> Result<Vec<u8>, ParseError> {
        if self.pos + count > self.data.len() {
            return Err(ParseError::Incomplete);
        }
        let vec = self.data[self.pos..self.pos + count].to_vec();
        self.pos += count;
        Ok(vec)
    }

    fn read_string(&mut self) -> Result<String, ParseError> {
        let len = self.read_u8()?;
        let bytes = self.read_vec(len as usize)?;
        String::from_utf8(bytes).map_err(|_| ParseError::InvalidUtf8)
    }

    #[allow(dead_code)]
    fn peek_u8(&self) -> Result<u8, ParseError> {
        if self.pos >= self.data.len() {
            return Err(ParseError::Incomplete);
        }
        Ok(self.data[self.pos])
    }

    #[allow(dead_code)]
    fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }
}

// Writer helper =============================================

struct Writer {
    data: Vec<u8>,
}

impl Writer {
    fn new() -> Self {
        Self { data: Vec::new() }
    }

    fn write_u8(&mut self, val: u8) {
        self.data.push(val);
    }

    fn write_magic(&mut self, magic: &[u8]) {
        self.data.extend_from_slice(magic);
    }

    fn write_slice(&mut self, slice: &[u8]) {
        self.data.extend_from_slice(slice);
    }

    fn write_string(&mut self, s: &str) {
        self.write_u8(s.len() as u8);
        self.write_slice(s.as_bytes());
    }

    fn into_vec(self) -> Vec<u8> {
        self.data
    }

    fn as_slice(&self) -> &[u8] {
        &self.data
    }
}

// Packet format ################################################

#[derive(Debug, PartialEq)]
struct AlwaysTen {}

impl AlwaysTen {
    fn parse(parser: &mut Parser) -> Result<Self, ParseError> {
        parser.read_magic(b"\x0A")?;
        Ok(AlwaysTen {})
    }

    fn write(&self, writer: &mut Writer) {
        writer.write_magic(b"\x0A");
    }
}

#[derive(Debug, PartialEq)]
pub struct ImprovWifiPacket {
    data_type: u8,
    data_length: u8,
    pub data: ImprovWifiPacketData,
    checksum: u8,
    always_ten: AlwaysTen,
}

impl ImprovWifiPacket {
    pub fn from_bytes(input: (&[u8], usize)) -> Result<((&[u8], usize), Self), ParseError> {
        let (input_data, bit_offset) = input;
        if bit_offset != 0 {
            // deku works with bit offsets, but we only support byte-aligned
            return Err(ParseError::Incomplete);
        }

        let mut parser = Parser::new(input_data);
        
        // Read magic
        parser.read_magic(b"IMPROV\x01")?;
        
        // Read data_type and data_length
        let data_type = parser.read_u8()?;
        let data_length = parser.read_u8()?;
        
        // Read data
        let data = ImprovWifiPacketData::parse(&mut parser, data_type)?;
        
        // Read checksum
        let checksum = parser.read_u8()?;
        
        // Read always_ten
        let always_ten = AlwaysTen::parse(&mut parser)?;
        
        // Verify checksum (all bytes except checksum and always_ten)
        let checksum_end = parser.pos - 2; // exclude checksum and 0x0A
        let calculated_checksum: u8 = input_data[..checksum_end]
            .iter()
            .fold(0, |acc, &x| acc.wrapping_add(x));
        
        if checksum != calculated_checksum {
            return Err(ParseError::InvalidChecksum);
        }
        
        let packet = ImprovWifiPacket {
            data_type,
            data_length,
            data,
            checksum,
            always_ten,
        };
        
        Ok(((&input_data[parser.pos..], 0), packet))
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

    pub fn to_bytes(&self) -> Result<Vec<u8>, ParseError> {
        let mut writer = Writer::new();
        
        // Write magic
        writer.write_magic(b"IMPROV\x01");
        
        // Write data_type (derived from data)
        let data_type = self.data.get_type_id();
        writer.write_u8(data_type);
        
        // Write data_length (derived from data)
        let data_length = self.data.get_data_length();
        writer.write_u8(data_length);
        
        // Write data
        self.data.write(&mut writer);
        
        // Calculate and write checksum (all bytes so far)
        let checksum: u8 = writer.as_slice()
            .iter()
            .fold(0, |acc, &x| acc.wrapping_add(x));
        writer.write_u8(checksum);
        
        // Write always_ten
        self.always_ten.write(&mut writer);
        
        Ok(writer.into_vec())
    }
}

#[derive(Debug, PartialEq)]
pub enum ImprovWifiPacketData {
    CurrentState(CurrentStateOption),
    ErrorState(ErrorStateOption),
    RPC(RPCCommandStruct),
    RPCResult(RPCResultStruct),
}

impl ImprovWifiPacketData {
    fn parse(parser: &mut Parser, data_type: u8) -> Result<Self, ParseError> {
        match data_type {
            0x01 => Ok(ImprovWifiPacketData::CurrentState(CurrentStateOption::parse(parser)?)),
            0x02 => Ok(ImprovWifiPacketData::ErrorState(ErrorStateOption::parse(parser)?)),
            0x03 => Ok(ImprovWifiPacketData::RPC(RPCCommandStruct::parse(parser)?)),
            0x04 => Ok(ImprovWifiPacketData::RPCResult(RPCResultStruct::parse(parser)?)),
            _ => Err(ParseError::InvalidDataType(data_type)),
        }
    }

    fn write(&self, writer: &mut Writer) {
        match self {
            ImprovWifiPacketData::CurrentState(s) => s.write(writer),
            ImprovWifiPacketData::ErrorState(s) => s.write(writer),
            ImprovWifiPacketData::RPC(s) => s.write(writer),
            ImprovWifiPacketData::RPCResult(s) => s.write(writer),
        }
    }

    pub fn get_data_length(&self) -> u8 {
        match self {
            ImprovWifiPacketData::CurrentState(current_state) => current_state.get_data_length(),
            ImprovWifiPacketData::ErrorState(error_state) => error_state.get_data_length(),
            ImprovWifiPacketData::RPC(rpc_command) => rpc_command.get_data_length(),
            ImprovWifiPacketData::RPCResult(rpc_result) => rpc_result.get_data_length(),
        }
    }

    fn get_type_id(&self) -> u8 {
        match self {
            ImprovWifiPacketData::CurrentState(_) => 0x01,
            ImprovWifiPacketData::ErrorState(_) => 0x02,
            ImprovWifiPacketData::RPC(_) => 0x03,
            ImprovWifiPacketData::RPCResult(_) => 0x04,
        }
    }
}

// Current State =================================

#[derive(Debug, PartialEq)]
pub enum CurrentStateOption {
    Ready,
    Provisioning,
    Provisioned,
}

impl CurrentStateOption {
    fn parse(parser: &mut Parser) -> Result<Self, ParseError> {
        let val = parser.read_u8()?;
        match val {
            0x02 => Ok(CurrentStateOption::Ready),
            0x03 => Ok(CurrentStateOption::Provisioning),
            0x04 => Ok(CurrentStateOption::Provisioned),
            _ => Err(ParseError::InvalidState(val)),
        }
    }

    fn write(&self, writer: &mut Writer) {
        let val = match self {
            CurrentStateOption::Ready => 0x02,
            CurrentStateOption::Provisioning => 0x03,
            CurrentStateOption::Provisioned => 0x04,
        };
        writer.write_u8(val);
    }

    pub fn get_data_length(&self) -> u8 {
        0x01
    }
}

// Error State =================================

#[derive(Debug, PartialEq)]
pub enum ErrorStateOption {
    NoError,
    InvalidRPCPacket,
    UnknownRPCCommand,
    UnableToConnect,
    UnknownError,
}

impl ErrorStateOption {
    fn parse(parser: &mut Parser) -> Result<Self, ParseError> {
        let val = parser.read_u8()?;
        match val {
            0x00 => Ok(ErrorStateOption::NoError),
            0x01 => Ok(ErrorStateOption::InvalidRPCPacket),
            0x02 => Ok(ErrorStateOption::UnknownRPCCommand),
            0x03 => Ok(ErrorStateOption::UnableToConnect),
            0xFF => Ok(ErrorStateOption::UnknownError),
            _ => Err(ParseError::InvalidError(val)),
        }
    }

    fn write(&self, writer: &mut Writer) {
        let val = match self {
            ErrorStateOption::NoError => 0x00,
            ErrorStateOption::InvalidRPCPacket => 0x01,
            ErrorStateOption::UnknownRPCCommand => 0x02,
            ErrorStateOption::UnableToConnect => 0x03,
            ErrorStateOption::UnknownError => 0xFF,
        };
        writer.write_u8(val);
    }

    pub fn get_data_length(&self) -> u8 {
        0x01
    }
}

// RPC Command ==============================

#[derive(Debug, PartialEq)]
pub struct RPCCommandStruct {
    command: u8,
    data_length: u8,
    pub data: RPCCommand,
}

impl RPCCommandStruct {
    fn parse(parser: &mut Parser) -> Result<Self, ParseError> {
        let command = parser.read_u8()?;
        let data_length = parser.read_u8()?;
        let data = RPCCommand::parse(parser, command)?;
        
        Ok(RPCCommandStruct {
            command,
            data_length,
            data,
        })
    }

    fn write(&self, writer: &mut Writer) {
        // Write command (derived from data)
        let command = self.data.get_command_id();
        writer.write_u8(command);
        
        // Write data_length (derived from data)
        let data_length = self.data.get_data_length();
        writer.write_u8(data_length);
        
        // Write data
        self.data.write(writer);
    }

    pub fn get_data_length(&self) -> u8 {
        2 + self.data.get_data_length()
    }
}

#[derive(Debug, PartialEq)]
#[allow(clippy::enum_variant_names)]
pub enum RPCCommand {
    SendWifiSettings(SendWifiSettingsStruct),
    RequestCurrentState,
    RequestDeviceInformation,
    RequestScannedWifiNetworks,
}

impl RPCCommand {
    fn parse(parser: &mut Parser, command: u8) -> Result<Self, ParseError> {
        match command {
            0x01 => Ok(RPCCommand::SendWifiSettings(SendWifiSettingsStruct::parse(parser)?)),
            0x02 => Ok(RPCCommand::RequestCurrentState),
            0x03 => Ok(RPCCommand::RequestDeviceInformation),
            0x04 => Ok(RPCCommand::RequestScannedWifiNetworks),
            _ => Err(ParseError::InvalidCommand(command)),
        }
    }

    fn write(&self, writer: &mut Writer) {
        match self {
            RPCCommand::SendWifiSettings(s) => s.write(writer),
            RPCCommand::RequestCurrentState => {},
            RPCCommand::RequestDeviceInformation => {},
            RPCCommand::RequestScannedWifiNetworks => {},
        }
    }

    pub fn get_data_length(&self) -> u8 {
        match self {
            RPCCommand::SendWifiSettings(send_wifi_settings) => {
                send_wifi_settings.get_data_length()
            }
            RPCCommand::RequestCurrentState => 0x00,
            RPCCommand::RequestDeviceInformation => 0x00,
            RPCCommand::RequestScannedWifiNetworks => 0x00,
        }
    }

    fn get_command_id(&self) -> u8 {
        match self {
            RPCCommand::SendWifiSettings(_) => 0x01,
            RPCCommand::RequestCurrentState => 0x02,
            RPCCommand::RequestDeviceInformation => 0x03,
            RPCCommand::RequestScannedWifiNetworks => 0x04,
        }
    }
}

// Send Wi-Fi settings -------------------------------------------------

#[derive(Debug, PartialEq)]
pub struct SendWifiSettingsStruct {
    pub ssid: String,
    pub password: String,
}

impl SendWifiSettingsStruct {
    fn parse(parser: &mut Parser) -> Result<Self, ParseError> {
        let ssid = parser.read_string()?;
        let password = parser.read_string()?;
        Ok(SendWifiSettingsStruct { ssid, password })
    }

    fn write(&self, writer: &mut Writer) {
        writer.write_string(&self.ssid);
        writer.write_string(&self.password);
    }

    fn get_data_length(&self) -> u8 {
        2 + self.ssid.len() as u8 + self.password.len() as u8
    }
}

// RPC Result ==============================================

#[derive(Debug, PartialEq)]
pub struct RPCResultStruct {
    command_responded: u8,
    strings_data_length: u8,
    strings: Vec<String>,
}

impl RPCResultStruct {
    fn parse(parser: &mut Parser) -> Result<Self, ParseError> {
        let command_responded = parser.read_u8()?;
        let strings_data_length = parser.read_u8()?;
        
        // Read strings until we've consumed strings_data_length bytes
        let mut strings = Vec::new();
        let start_pos = parser.pos;
        
        while parser.pos - start_pos < strings_data_length as usize {
            strings.push(parser.read_string()?);
        }
        
        Ok(RPCResultStruct {
            command_responded,
            strings_data_length,
            strings,
        })
    }

    fn write(&self, writer: &mut Writer) {
        writer.write_u8(self.command_responded);
        
        // Write strings_data_length (calculated)
        let strings_data_length = Self::get_strings_data_length(&self.strings);
        writer.write_u8(strings_data_length);
        
        // Write all strings
        for s in &self.strings {
            writer.write_string(s);
        }
    }

    pub fn get_data_length(&self) -> u8 {
        let len: u8 = 1/*command_responded byte */+1 /*data_length byte*/ + Self::get_strings_data_length(&self.strings);
        len
    }

    fn get_strings_data_length(data: &[String]) -> u8 {
        let value: u8 = data
            .iter()
            .fold(0, |acc, x| acc + 1/*string len byte*/ + x.len() as u8);
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
            command_responded: 0x03, // RequestDeviceInformation
            strings_data_length: 0x00,
            strings: vec![
                firmware_name.to_string(),
                firmware_version.to_string(),
                chip.to_string(),
                device_name.to_string(),
            ],
        }
    }

    pub fn new_response_to_request_scanned_wifi_networks(
        ssid: &str,
        rssi: &str,
        auth_required: bool,
    ) -> Self {
        Self {
            command_responded: 0x04, // RequestScannedWifiNetworks
            strings_data_length: 0x00,
            strings: vec![
                ssid.to_string(),
                rssi.to_string(),
                if auth_required {
                    "YES".to_string()
                } else {
                    "NO".to_string()
                },
            ],
        }
    }

    pub fn new_response_to_request_scanned_wifi_networks_end() -> Self {
        Self {
            command_responded: 0x04, // RequestScannedWifiNetworks
            strings_data_length: 0x00,
            strings: vec![],
        }
    }

    pub fn new_response_to_send_wifi_settings(redirect_url: &str) -> Self {
        Self {
            command_responded: 0x01, // SendWifiSettings
            strings_data_length: 0x00,
            strings: vec![redirect_url.to_string()],
        }
    }
}
