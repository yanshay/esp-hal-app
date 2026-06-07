use aes::cipher::{KeyIvInit, StreamCipher};
use aes_gcm::{
    aead::{Aead, AeadInPlace, KeyInit, Payload},
    Aes256Gcm, Key, Nonce, Tag,
};
use alloc::{
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};
use base64::{encoded_len, engine::general_purpose::STANDARD_NO_PAD, Engine as _};
use hmac::{Hmac, Mac};
use pbkdf2::pbkdf2_hmac;
use picoserve::{
    io::Read,
    response::{IntoResponse, StatusCode},
    ResponseSent,
};
use serde::Serialize;
use sha2::Sha256;

use super::runtime::BodyReadRejection;

// Macro has to be used prior to usage, it is for encryption reasons (encryption code comes later)
#[macro_export]
macro_rules! encrypted_input {
    ($type:ident) => {
        impl<'r, MoreState> FromRequest<'r, WebAppState<MoreState>> for $type {
            type Rejection = EncryptedRejection;

            async fn from_request<R: Read>(
                state: &'r WebAppState<MoreState>,
                _request_parts: RequestParts<'r>,
                request_body: RequestBody<'r, R>,
            ) -> Result<Self, Self::Rejection> {
                let encrypted_data = $crate::framework_web_app::read_limited_body(
                    request_body,
                    state.request_body_max_bytes,
                )
                .await?;
                let key = state.encryption.0;
                let decrypted_data =
                    $crate::framework_web_app::decrypt_compact(&key.borrow(), encrypted_data)
                        .map_err(|e| EncryptedRejection::DecryptionError(e))?;

                (serde_json::from_str(&decrypted_data) as Result<$type, _>)
                    .map_err(|e| EncryptedRejection::DeserializationError(e))
            }
        }
    };
}

#[macro_export]
macro_rules! not_encrypted_input {
    ($type:ident) => {
        impl<'r, MoreState> FromRequest<'r, WebAppState<MoreState>> for $type {
            type Rejection = EncryptedRejection;

            async fn from_request<R: Read>(
                state: &'r WebAppState<MoreState>,
                _request_parts: RequestParts<'r>,
                request_body: RequestBody<'r, R>,
            ) -> Result<Self, Self::Rejection> {
                let raw_input = $crate::framework_web_app::read_limited_body(
                    request_body,
                    state.request_body_max_bytes,
                )
                .await?;

                (serde_json::from_slice(&raw_input) as Result<$type, _>)
                    .map_err(|e| EncryptedRejection::DeserializationError(e))
            }
        }
    };
}

/////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////
// AES-GCM Encryption ///////////////////////////////////////////////////////////////////////////////////////////////////////////////
/////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

pub fn derive_key(key: &str, salt: &[u8], iterations: u32) -> Vec<u8> {
    let mut key_bytes = vec![0u8; 32]; // 32-byte key for AES-256
    pbkdf2_hmac::<Sha256>(key.as_bytes(), salt, iterations, &mut key_bytes);
    key_bytes
}

pub fn encrypt_bytes(key_bytes: &[u8], data: &[u8]) -> String {
    // Derive key (32 bytes from a user-provided key)

    assert!(!key_bytes.is_empty());
    // let key_bytes = derive_key(key);
    let key = Key::<Aes256Gcm>::from_slice(key_bytes);

    let cipher = Aes256Gcm::new(key);

    // Generate random IV (12 bytes for AES-GCM)
    let mut iv_bytes = [0u8; 12];
    getrandom::getrandom(&mut iv_bytes).expect("Random should not fail");
    let iv = Nonce::from_slice(&iv_bytes);

    // Encrypt the data
    let ciphertext = cipher
        .encrypt(iv, Payload::from(data))
        .expect("Encryption here should not fail"); // only memory issue?
    let res = format!(
        "{}{}",
        STANDARD_NO_PAD.encode(iv),
        STANDARD_NO_PAD.encode(ciphertext)
    );

    res
}

pub fn encrypt_bytes_compact<E, F>(
    key_bytes: &[u8],
    plaintext_len: usize,
    write_plaintext: F,
) -> Result<String, E>
where
    F: FnOnce(&mut [u8]) -> Result<(), E>,
{
    const IV_LEN: usize = 12;
    const TAG_LEN: usize = 16;
    const IV_B64_LEN: usize = 16;
    const OVERLAP_SLACK: usize = 64;

    assert!(!key_bytes.is_empty());

    let ciphertext_len = plaintext_len
        .checked_add(TAG_LEN)
        .expect("usize overflow when calculating ciphertext size");
    let ciphertext_b64_len =
        encoded_len(ciphertext_len, false).expect("usize overflow when calculating base64 size");
    let final_len = IV_B64_LEN
        .checked_add(ciphertext_b64_len)
        .expect("usize overflow when calculating encrypted size");
    let working_len = final_len
        .checked_add(OVERLAP_SLACK)
        .expect("usize overflow when calculating encrypted buffer size");
    let mut buffer = vec![0_u8; working_len];

    let mut iv_bytes = [0u8; IV_LEN];
    getrandom::getrandom(&mut iv_bytes).expect("Random should not fail");
    let iv = Nonce::from_slice(&iv_bytes);
    let iv_len = STANDARD_NO_PAD
        .encode_slice(iv_bytes, &mut buffer[..IV_B64_LEN])
        .expect("IV base64 buffer should be large enough");
    debug_assert_eq!(iv_len, IV_B64_LEN);

    let ciphertext_start = working_len - ciphertext_len;
    let tag_start = ciphertext_start + plaintext_len;
    write_plaintext(&mut buffer[ciphertext_start..tag_start])?;

    let key = Key::<Aes256Gcm>::from_slice(key_bytes);
    let cipher = Aes256Gcm::new(key);
    let tag = cipher
        .encrypt_in_place_detached(iv, b"", &mut buffer[ciphertext_start..tag_start])
        .expect("Encryption here should not fail");
    buffer[tag_start..tag_start + TAG_LEN].copy_from_slice(tag.as_slice());

    // SAFETY: this intentionally overlaps source and destination to avoid a second full-size
    // response allocation. The ciphertext is placed after the output start, and the slack keeps the
    // current base64 encoder's forward writes behind bytes it has not read yet.
    let bytes_written = unsafe {
        let ptr = buffer.as_mut_ptr();
        let ciphertext = core::slice::from_raw_parts(ptr.add(ciphertext_start), ciphertext_len);
        let ciphertext_b64 =
            core::slice::from_raw_parts_mut(ptr.add(IV_B64_LEN), ciphertext_b64_len);
        STANDARD_NO_PAD
            .encode_slice(ciphertext, ciphertext_b64)
            .expect("ciphertext base64 buffer should be large enough")
    };
    debug_assert_eq!(bytes_written, ciphertext_b64_len);

    buffer.truncate(final_len);
    Ok(unsafe { String::from_utf8_unchecked(buffer) })
}

pub fn encrypt(key_bytes: &[u8], data: &str) -> String {
    encrypt_bytes(key_bytes, data.as_bytes())
}

pub fn decrypt(key_bytes: &[u8], encrypted: &[u8]) -> Result<String, String> {
    //Derive key (32 bytes from a user-provided key)
    // let key_bytes = derive_key(key);
    let key = Key::<Aes256Gcm>::from_slice(key_bytes);

    let cipher = Aes256Gcm::new(key);

    // Decode IV and ciphertext
    if encrypted.len() < 16 {
        return Err("Improperly encrypted or not encrypted data".to_string());
    }
    let iv_bytes = STANDARD_NO_PAD
        .decode(&encrypted[..16])
        .map_err(|_| "Failed to decode IV".to_string())?;
    let iv = Nonce::from_slice(&iv_bytes);

    let ciphertext = STANDARD_NO_PAD
        .decode(&encrypted[16..])
        .map_err(|_| "Failed to decode ciphertext".to_string())?;

    // Decrypt the data
    let plaintext = cipher
        .decrypt(iv, Payload::from(&ciphertext[..])) // Use `&ciphertext[..]` here
        .map_err(|e| format!("Decryption failed : {e}"))?;

    String::from_utf8(plaintext).map_err(|_| "Failed to convert plaintext to string".to_string())
}

fn decode_base64_no_pad_byte(byte: u8) -> Result<u8, String> {
    match byte {
        b'A'..=b'Z' => Ok(byte - b'A'),
        b'a'..=b'z' => Ok(byte - b'a' + 26),
        b'0'..=b'9' => Ok(byte - b'0' + 52),
        b'+' => Ok(62),
        b'/' => Ok(63),
        _ => Err("Invalid base64 data".to_string()),
    }
}

fn decode_base64_no_pad_to_start(
    buffer: &mut Vec<u8>,
    input_start: usize,
) -> Result<usize, String> {
    if input_start > buffer.len() {
        return Err("Invalid base64 input offset".to_string());
    }

    let input_len = buffer.len() - input_start;
    let remainder = input_len % 4;
    if remainder == 1 {
        return Err("Invalid base64 data length".to_string());
    }

    let end = buffer.len();
    let full_chunk_end = end - remainder;
    let mut read_pos = input_start;
    let mut write_pos = 0;

    while read_pos < full_chunk_end {
        let a = decode_base64_no_pad_byte(buffer[read_pos])?;
        let b = decode_base64_no_pad_byte(buffer[read_pos + 1])?;
        let c = decode_base64_no_pad_byte(buffer[read_pos + 2])?;
        let d = decode_base64_no_pad_byte(buffer[read_pos + 3])?;

        buffer[write_pos] = (a << 2) | (b >> 4);
        buffer[write_pos + 1] = (b << 4) | (c >> 2);
        buffer[write_pos + 2] = (c << 6) | d;

        read_pos += 4;
        write_pos += 3;
    }

    if remainder == 2 {
        let a = decode_base64_no_pad_byte(buffer[read_pos])?;
        let b = decode_base64_no_pad_byte(buffer[read_pos + 1])?;
        if b & 0x0f != 0 {
            return Err("Invalid base64 data".to_string());
        }
        buffer[write_pos] = (a << 2) | (b >> 4);
        write_pos += 1;
    } else if remainder == 3 {
        let a = decode_base64_no_pad_byte(buffer[read_pos])?;
        let b = decode_base64_no_pad_byte(buffer[read_pos + 1])?;
        let c = decode_base64_no_pad_byte(buffer[read_pos + 2])?;
        if c & 0x03 != 0 {
            return Err("Invalid base64 data".to_string());
        }
        buffer[write_pos] = (a << 2) | (b >> 4);
        buffer[write_pos + 1] = (b << 4) | (c >> 2);
        write_pos += 2;
    }

    buffer.truncate(write_pos);
    Ok(write_pos)
}

pub fn decrypt_compact(key_bytes: &[u8], mut encrypted: Vec<u8>) -> Result<String, String> {
    const IV_B64_LEN: usize = 16;
    const IV_LEN: usize = 12;
    const TAG_LEN: usize = 16;

    assert!(!key_bytes.is_empty());
    if encrypted.len() < IV_B64_LEN {
        return Err("Improperly encrypted or not encrypted data".to_string());
    }

    let mut iv_bytes = [0u8; IV_LEN];
    let iv_len = STANDARD_NO_PAD
        .decode_slice(&encrypted[..IV_B64_LEN], &mut iv_bytes)
        .map_err(|_| "Failed to decode IV".to_string())?;
    if iv_len != IV_LEN {
        return Err("Failed to decode IV".to_string());
    }
    let iv = Nonce::from_slice(&iv_bytes);

    let decoded_len = decode_base64_no_pad_to_start(&mut encrypted, IV_B64_LEN)
        .map_err(|_| "Failed to decode ciphertext".to_string())?;
    if decoded_len < TAG_LEN {
        return Err("Improperly encrypted or not encrypted data".to_string());
    }

    let plaintext_len = decoded_len - TAG_LEN;
    let mut tag_bytes = [0u8; TAG_LEN];
    tag_bytes.copy_from_slice(&encrypted[plaintext_len..decoded_len]);

    let key = Key::<Aes256Gcm>::from_slice(key_bytes);
    let cipher = Aes256Gcm::new(key);
    let tag = Tag::from_slice(&tag_bytes);
    cipher
        .decrypt_in_place_detached(iv, b"", &mut encrypted[..plaintext_len], tag)
        .map_err(|e| format!("Decryption failed : {e}"))?;

    encrypted.truncate(plaintext_len);
    String::from_utf8(encrypted).map_err(|_| "Failed to convert plaintext to string".to_string())
}

pub trait Encryptable<T: Serialize> {
    // fn encrypt(&self, key: &[u8], rng: Rng) -> EncryptedData;
    fn encrypt(&self, key: &[u8]) -> String;
}

impl<T> Encryptable<T> for T
where
    T: Serialize,
{
    fn encrypt(&self, key: &[u8]) -> String {
        let serialized = serde_json::to_string(self).expect("Serialization failed");
        encrypt(key, &serialized)
    }
}

#[derive(Debug)]
pub enum EncryptedRejection {
    BodyRead(BodyReadRejection),
    DecryptionError(String),
    DeserializationError(serde_json::Error),
}

impl From<BodyReadRejection> for EncryptedRejection {
    fn from(value: BodyReadRejection) -> Self {
        Self::BodyRead(value)
    }
}

impl IntoResponse for EncryptedRejection {
    async fn write_to<R: Read, W: picoserve::response::ResponseWriter<Error = R::Error>>(
        self,
        connection: picoserve::response::Connection<'_, R>,
        response_writer: W,
    ) -> Result<ResponseSent, W::Error> {
        match self {
            Self::BodyRead(error) => error.write_to(connection, response_writer).await,
            Self::DeserializationError(error) => {
                (
                    StatusCode::BAD_REQUEST,
                    format_args!("Failed to parse JSON body: {error}"),
                )
                    .write_to(connection, response_writer)
                    .await
            }
            Self::DecryptionError(error) => {
                (
                    StatusCode::BAD_REQUEST,
                    format_args!("Failed to decrypt data: {error}"),
                )
                    .write_to(connection, response_writer)
                    .await
            }
        }
    }
}

/////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////
// AES-CTR Encryption ///////////////////////////////////////////////////////////////////////////////////////////////////////////////
/////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

type Aes256Ctr32BE = ctr::Ctr32BE<aes::Aes256>; // The 32 and BE are important for compatibility with CryptoJS

fn ctr_encrypt(key_bytes: &[u8], data: &str) -> String {
    let mut key = [0u8; 32];
    key.copy_from_slice(key_bytes);

    let mut iv = [0x24; 16]; // random, sent with data
    getrandom::getrandom(&mut iv).unwrap();

    let mut cipher = Aes256Ctr32BE::new(&key.into(), &iv.into());

    let mut dest = data.as_bytes().to_vec();
    cipher.apply_keystream(&mut dest);

    let encrypted_content = format!(
        "{}{}",
        STANDARD_NO_PAD.encode(iv).trim_end_matches('='),
        STANDARD_NO_PAD.encode(dest).trim_end_matches('=')
    );

    // calculate hmac tag prefix
    let mut hmac = <Hmac<Sha256> as KeyInit>::new_from_slice(&key).expect("Invalid key length");
    hmac.update(encrypted_content.as_bytes());
    let hmac_tag = STANDARD_NO_PAD.encode(hmac.finalize().into_bytes().as_slice()); // sha 256: 32 bytes -> 43 base64 no padding
    format!("{hmac_tag}{encrypted_content}")
}

pub(super) fn ctr_decrypt(key_bytes: &[u8], encrypted: &[u8]) -> Result<String, String> {
    // start verifying the hmac tag

    let hmac_base64 = core::str::from_utf8(&encrypted[..43])
        .map_err(|e| format!("Failed UTF8 decoding hmac {e}"))?;
    let received_hmac = STANDARD_NO_PAD
        .decode(hmac_base64)
        .map_err(|e| format!("Failed BASE64 decoding hmac {e}"))?;

    let encrypted_content = &encrypted[43..];

    let mut hmac =
        <Hmac<Sha256> as KeyInit>::new_from_slice(key_bytes).expect("Invalid key length");
    hmac.update(encrypted_content);
    let calced_hmac = hmac.finalize().into_bytes();
    let calced_hmac = calced_hmac.as_slice(); // sha 256: 32 bytes -> 43 base64 no padding

    if received_hmac != calced_hmac {
        return Err("Failed hmac validation".to_string());
    }

    let encrypted = encrypted_content;

    // decrypt

    let mut key = [0u8; 32];
    key.copy_from_slice(key_bytes);

    // Decode IV and ciphertext
    let iv_vec = STANDARD_NO_PAD
        .decode(&encrypted[..22])
        .map_err(|e| format!("Failed to decode IV: {e}"))?;
    let iv: &[u8; 16] = iv_vec.as_slice().try_into().unwrap();

    let mut cipher = Aes256Ctr32BE::new(&key.into(), iv.into());

    let mut dest = STANDARD_NO_PAD
        .decode(&encrypted[22..])
        .map_err(|_| "Failed to decode data".to_string())?;

    for chunk in dest.chunks_mut(1) {
        cipher
            .try_apply_keystream(chunk)
            .map_err(|e| format!("Decryption error {e}"))?;
    }
    String::from_utf8(dest).map_err(|_| "Failed to convert plaintext to string".to_string())
}

pub trait EncryptableCTR {
    // fn encrypt(&self, key: &[u8], rng: Rng) -> EncryptedData;
    // fn encrypt(&self, key: &[u8]) -> String;
    fn ctr_encrypt(&self, key: &[u8]) -> String
    where
        Self: Serialize,
    {
        let serialized = serde_json::to_string(self).expect("Serialization failed");
        ctr_encrypt(key, &serialized)
    }
}
