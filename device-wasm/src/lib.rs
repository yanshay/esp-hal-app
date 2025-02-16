mod utils;

use wasm_bindgen::prelude::*;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce}; // AES-GCM implementation
use base64::{engine::general_purpose::STANDARD_NO_PAD, Engine as _};
use pbkdf2::pbkdf2_hmac;
// use serde::{Deserialize, Serialize};
use sha2::Sha256;
use web_sys::js_sys::Object;

#[wasm_bindgen]
extern "C" {
    fn alert(s: &str);
}

#[wasm_bindgen]
pub fn greet() {
    alert("Hello, hello-wasm-changed!");
}

#[wasm_bindgen]
pub fn greettext(s: &str) -> String {
    alert(s);
    "Return value 5".to_string()
}

// #[derive(Debug, Serialize, Deserialize)]
// struct EncryptedData {
//     ciphertext: String,
//     iv: String,
// }

pub fn fill_bytes(buf: &mut [u8]) -> Result<Object, JsValue> {
    let crypto = web_sys::window()
        .ok_or_else(|| "No window")?
        .crypto()
        .map_err(|e| format!("No crypto: {e:?}"))?;

    crypto.get_random_values_with_u8_array(buf)
}

#[wasm_bindgen]
pub fn derive_key(key: &str, salt: &str) -> Vec<u8> {
    let salt = salt.as_bytes();

    let mut key_bytes = vec![0u8; 32]; // 32-byte key for AES-256
    pbkdf2_hmac::<Sha256>(key.as_bytes(), salt, 10_000, &mut key_bytes);

    key_bytes
}


#[wasm_bindgen]
pub fn decrypt(key_bytes: &[u8], encrypted: &str) -> Result<String, JsValue> {
    // Derive key (32 bytes from a user-provided key)
    let key = Key::<Aes256Gcm>::from_slice(&key_bytes);

    let cipher = Aes256Gcm::new(key);

    // Decode IV and ciphertext
    let iv_bytes = STANDARD_NO_PAD
        .decode(&encrypted[0..16])
        .map_err(|e| format!("Failed to decode IV: {e}"))?;
    let iv = Nonce::from_slice(&iv_bytes);

    let ciphertext = STANDARD_NO_PAD
        .decode(&encrypted[16..])
        .map_err(|e| format!("Failed to decode ciphertext: {e}"))?;

    // Decrypt the data
    let plaintext = cipher.decrypt(iv, Payload::from(&ciphertext[..]));

    let plaintext = plaintext.map_err(|e| format!("Decryption failed: {e}"))?;

    Ok(String::from_utf8(plaintext).map_err(|_| "Failed to convert plaintext to string")?)
}

#[wasm_bindgen]
pub fn encrypt(key_bytes: &[u8], data: &str) -> Result<String, JsValue> {
    let key = Key::<Aes256Gcm>::from_slice(&key_bytes);

    let cipher = Aes256Gcm::new(key);

    // Generate random IV (12 bytes for AES-GCM)
    let mut iv_bytes = [0u8; 12];
    fill_bytes(&mut iv_bytes);
    let iv = Nonce::from_slice(&iv_bytes);

    // Encrypt the data
    let ciphertext = cipher
        .encrypt(iv, Payload::from(data.as_bytes()))
        .map_err(|e| format!("Encryption failed: {e}"))?;

    Ok(format!(
        "{}{}",
        STANDARD_NO_PAD.encode(&iv_bytes),
        STANDARD_NO_PAD.encode(&ciphertext),
    ))
}

// #[wasm_bindgen]
// pub fn old_decrypt(key_bytes: &[u8], encrypted: &str) -> Result<String, JsValue> {
//     let encrypted: EncryptedData =
//         serde_json::from_str(encrypted).map_err(|e| format!("Bad encrypted format: {e}"))?;
//
//     // Derive key (32 bytes from a user-provided key)
//     let key = Key::<Aes256Gcm>::from_slice(&key_bytes);
//
//     let cipher = Aes256Gcm::new(key);
//
//     // Decode IV and ciphertext
//     let iv_bytes = STANDARD
//         .decode(&encrypted.iv)
//         .map_err(|e| format!("Failed to decode IV: {e}"))?;
//     let iv = Nonce::from_slice(&iv_bytes);
//
//     let ciphertext = STANDARD
//         .decode(&encrypted.ciphertext)
//         .map_err(|e| format!("Failed to decode ciphertext: {e}"))?;
//
//     // Decrypt the data
//     let plaintext = cipher.decrypt(iv, Payload::from(&ciphertext[..]));
//
//     let plaintext = plaintext.map_err(|e| format!("Decryption failed: {e}"))?;
//
//     Ok(String::from_utf8(plaintext).map_err(|_| "Failed to convert plaintext to string")?)
// }
//
// #[wasm_bindgen]
// pub fn old_encrypt(key_bytes: &[u8], data: &str) -> Result<String, JsValue> {
//     let key = Key::<Aes256Gcm>::from_slice(&key_bytes);
//
//     let cipher = Aes256Gcm::new(key);
//
//     // Generate random IV (12 bytes for AES-GCM)
//     let mut iv_bytes = [0u8; 12];
//     fill_bytes(&mut iv_bytes);
//     let iv = Nonce::from_slice(&iv_bytes);
//
//     // Encrypt the data
//     let ciphertext = cipher
//         .encrypt(iv, Payload::from(data.as_bytes()))
//         .map_err(|e| format!("Encryption failed: {e}"))?;
//
//     Ok(format!(
//         "{{\"ciphertext\": \"{}\", \"iv\": \"{}\"}}",
//         STANDARD.encode(&ciphertext),
//         STANDARD.encode(&iv_bytes) // encode(&ciphertext),
//                                    // encode(&iv_bytes)
//     ))
// }
