use alloc::{
    string::{String, ToString},
    vec::Vec,
};
use base64::{engine::general_purpose::URL_SAFE, Engine as _};
use esp_partition_table::PartitionTable;

use hashbrown::HashMap;
use pasetors::{
    keys::AsymmetricPublicKey,
    token::UntrustedToken,
    version4::{self, V4},
    Public,
};
use serde_json::Value;


#[derive(serde::Deserialize, serde::Serialize)]
struct License {
    version: String,
    // encoded mac address
    mac_addr: String,
}

pub struct LicenseManager {
    license: License,
}

impl LicenseManager {
    pub fn new() -> Self {
        Self {
            license: License { version: String::new(), mac_addr: String::new() },
        }
    }

    pub fn load_license<S: embedded_storage::ReadStorage>(&mut self, flash_storage: &mut S, magic: &str, public_key: &str, obfuscate_key: &str) -> Result<(), String> {
        let partition_table = PartitionTable::default();

        let mut lic_start: Option<u32> = None;
        let mut lic_end: Option<u32> = None;
        partition_table.iter_storage(flash_storage, false).for_each(|partition| {
            if let Ok(partition) = partition {
                if partition.name() == "lic" {
                    lic_start = Some(partition.offset);
                    lic_end = Some(partition.offset + partition.size as u32);
                }
            }
        });

        let lic_start = lic_start.ok_or(String::from("Flash region is missing"))?;

        // Get Token
        let mut header = [0u8; 8 + 2];
        flash_storage
            .read(lic_start, header.as_mut_slice())
            .map_err(|_| String::from("Can't read flash"))?;
        if header[..magic.len()] != *magic.as_bytes() {
            return Err(String::from("No license available"));
        };
        let token_len: u16 = u16::from_le_bytes(header[8..10].try_into().unwrap());
        let mut xored_token_bytes = alloc::vec![0u8;token_len.into()];
        flash_storage
            .read(lic_start + header.len() as u32, &mut xored_token_bytes)
            .map_err(|_| String::from("Error reading from flash"))?;
        let xored_token_str = core::str::from_utf8(&xored_token_bytes).map_err(|_| String::from("Decoding failure (1)"))?;
        let pub_token = decode_with_xor(xored_token_str, obfuscate_key.as_bytes()).map_err(|_| String::from("Decoding failure (2)"))?;

        // Get Public Key
        let key_bytes = URL_SAFE.decode(public_key).unwrap();
        let key = AsymmetricPublicKey::<V4>::from(&key_bytes).unwrap();

        // Verify Token
        let untrusted_token = UntrustedToken::<Public, V4>::try_from(&pub_token).map_err(|_| String::from("Decoding failure (3)"))?;

        let trusted_token = version4::PublicToken::verify(&key, &untrusted_token, None, None).map_err(|_| String::from("Verification error"))?;

        let claims_list: HashMap<String, Value> = serde_json::from_str(trusted_token.payload()).map_err(|_| String::from("Parsing error"))?;

        let license_str = claims_list
            .get("license")
            .ok_or(String::from("Missing information"))?
            .as_str()
            .ok_or(String::from("Bad information (1)"))?;

        self.license = serde_json::from_str::<License>(license_str).map_err(|_| String::from("Bad information (2)"))?;

        Ok(())
    }

    pub fn is_license_ok(&self) -> Result<bool, String> {
        let mac_vec = URL_SAFE
            .decode(self.license.mac_addr.as_bytes())
            .map_err(|_| String::from("Decoding device information error"))?;
        let license_mac_addr: [u8; 6] = mac_vec.try_into().map_err(|_| String::from("Bad device information"))?;

        let device_mac_addr = esp_hal::efuse::Efuse::mac_address();

        if device_mac_addr == license_mac_addr {
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

fn xor(data: &[u8], key: &[u8]) -> Vec<u8> {
    data.iter()
        .enumerate()
        .map(|(i, &byte)| byte ^ key[i % key.len()]) // XOR with key (repeats if key is shorter)
        .collect()
}

/// Encode data using XOR and Base64
#[allow(dead_code)]
fn encode_with_xor(input: &str, key: &[u8]) -> String {
    // Step 1: XOR the input data
    let xor_result = xor(input.as_bytes(), key);

    // Step 2: Base64 encode the XOR result
    URL_SAFE.encode(&xor_result)
}
/// Decode data from Base64 and XOR

fn decode_with_xor(encoded: &str, key: &[u8]) -> Result<String, base64::DecodeError> {
    // Step 1: Base64 decode the input
    // TODO: deal with error handling, for some reason doesn't automatically convert error
    let decoded = URL_SAFE.decode(encoded).unwrap();

    // Step 2: XOR the decoded data
    let original = xor(&decoded, key);

    // Convert back to a UTF-8 string
    Ok(String::from_utf8_lossy(&original).to_string())
}
