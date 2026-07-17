//! Self-describing sealed-envelope codec (Issue #3359, slice PR-1a).
//!
//! Wire format (modeled on the cold-tier `ACV1` loud-fail wrapper):
//!
//! ```text
//! SUBJ (4) | fmt_ver:u8 | alg_id:u8 | key_version:u32 LE | subj_len:u16 LE | subject_id | AEAD_blob
//! ```
//!
//! The `AEAD_blob` is `[nonce || ciphertext || tag]` produced by the subject
//! cipher. The AEAD **AAD binds the subject id + key version**, so an envelope
//! only decrypts under its own subject's key at its own key version — a
//! cross-entity swap (moving an envelope onto another subject) is a loud AEAD
//! auth failure, never fabricated plaintext.

use crate::core::property::PropertyValue;
use crate::encryption::Cipher;

use super::error::CryptoShredError;

/// Magic prefix identifying a sealed subject envelope.
pub const ENVELOPE_MAGIC: &[u8; 4] = b"SUBJ";

/// Current envelope format version.
pub const ENVELOPE_FORMAT_VERSION: u8 = 1;

/// Fixed-size header length preceding the variable subject id + AEAD blob:
/// magic(4) + fmt_ver(1) + alg_id(1) + key_version(4) + subj_len(2) = 12.
pub const ENVELOPE_HEADER_LEN: usize = 4 + 1 + 1 + 4 + 2;

/// AAD domain-separation prefix (bound alongside key version + subject id).
const AAD_DOMAIN: &[u8] = b"aletheiadb-subject-envelope-v1";

/// Build the AEAD AAD binding this envelope to its subject id + key version.
fn envelope_aad(subject_id: &[u8], key_version: u32) -> Vec<u8> {
    let mut aad = Vec::with_capacity(AAD_DOMAIN.len() + 4 + subject_id.len());
    aad.extend_from_slice(AAD_DOMAIN);
    aad.extend_from_slice(&key_version.to_le_bytes());
    aad.extend_from_slice(subject_id);
    aad
}

/// Seal `plaintext` into a self-describing envelope under `cipher`.
///
/// `subject_id` and `key_version` are stamped into the header AND bound as AEAD
/// AAD. `cipher` must be the subject's own cipher (built from the unwrapped
/// subject DEK).
///
/// # Errors
/// [`CryptoShredError::InvalidArgument`] if `subject_id` is longer than `u16`
/// can express, or [`CryptoShredError::Crypto`] on AEAD failure.
pub fn seal(
    plaintext: &[u8],
    subject_id: &str,
    key_version: u32,
    cipher: &dyn Cipher,
) -> Result<Vec<u8>, CryptoShredError> {
    let subj = subject_id.as_bytes();
    let subj_len: u16 = subj.len().try_into().map_err(|_| {
        CryptoShredError::InvalidArgument("subject id too long to seal".to_string())
    })?;

    let aad = envelope_aad(subj, key_version);
    let blob = cipher
        .encrypt(plaintext, &aad)
        .map_err(|e| CryptoShredError::Crypto(e.to_string()))?;

    let mut out = Vec::with_capacity(ENVELOPE_HEADER_LEN + subj.len() + blob.len());
    out.extend_from_slice(ENVELOPE_MAGIC);
    out.push(ENVELOPE_FORMAT_VERSION);
    out.push(cipher.algorithm_id());
    out.extend_from_slice(&key_version.to_le_bytes());
    out.extend_from_slice(&subj_len.to_le_bytes());
    out.extend_from_slice(subj);
    out.extend_from_slice(&blob);
    Ok(out)
}

/// Parsed header of a sealed envelope (excludes the AEAD blob).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvelopeHeader {
    /// Format version.
    pub format_version: u8,
    /// AEAD algorithm id the envelope was sealed with.
    pub algorithm_id: u8,
    /// Key version of the subject key that sealed this envelope.
    pub key_version: u32,
    /// The subject id this envelope belongs to.
    pub subject_id: String,
}

/// `true` if `bytes` begins with the sealed-envelope magic.
#[must_use]
pub fn is_envelope(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && &bytes[0..4] == ENVELOPE_MAGIC
}

/// Parse the header and return it plus the trailing AEAD blob slice.
fn parse(envelope: &[u8]) -> Result<(EnvelopeHeader, &[u8]), CryptoShredError> {
    if envelope.len() < ENVELOPE_HEADER_LEN {
        return Err(CryptoShredError::Crypto(
            "sealed envelope truncated".to_string(),
        ));
    }
    if &envelope[0..4] != ENVELOPE_MAGIC {
        return Err(CryptoShredError::Crypto(
            "not a sealed envelope".to_string(),
        ));
    }
    let format_version = envelope[4];
    if format_version != ENVELOPE_FORMAT_VERSION {
        return Err(CryptoShredError::Crypto(format!(
            "unsupported sealed-envelope format version {format_version}"
        )));
    }
    let algorithm_id = envelope[5];
    let key_version = u32::from_le_bytes([envelope[6], envelope[7], envelope[8], envelope[9]]);
    let subj_len = u16::from_le_bytes([envelope[10], envelope[11]]) as usize;

    let subj_start = ENVELOPE_HEADER_LEN;
    let subj_end = subj_start
        .checked_add(subj_len)
        .ok_or_else(|| CryptoShredError::Crypto("sealed envelope length overflow".to_string()))?;
    if envelope.len() < subj_end {
        return Err(CryptoShredError::Crypto(
            "sealed envelope truncated".to_string(),
        ));
    }
    let subject_id = std::str::from_utf8(&envelope[subj_start..subj_end])
        .map_err(|_| CryptoShredError::Crypto("sealed envelope subject id not utf-8".to_string()))?
        .to_string();
    let blob = &envelope[subj_end..];
    Ok((
        EnvelopeHeader {
            format_version,
            algorithm_id,
            key_version,
            subject_id,
        },
        blob,
    ))
}

/// Parse a sealed envelope's header without decrypting (read-path routing).
///
/// # Errors
/// [`CryptoShredError::Crypto`] if the bytes are not a well-formed envelope.
pub fn parse_header(envelope: &[u8]) -> Result<EnvelopeHeader, CryptoShredError> {
    parse(envelope).map(|(h, _)| h)
}

/// Unseal an envelope produced by [`seal`], returning the original plaintext.
///
/// The AAD (subject id + key version) is reconstructed from the parsed header,
/// so a mismatch — a tampered header, a swapped subject, or the wrong key —
/// fails the AEAD auth check loudly rather than fabricating a value.
///
/// # Errors
/// [`CryptoShredError::Crypto`] if the envelope is malformed or AEAD auth fails
/// (including "wrong / destroyed key").
pub fn unseal(envelope: &[u8], cipher: &dyn Cipher) -> Result<Vec<u8>, CryptoShredError> {
    let (header, blob) = parse(envelope)?;
    let aad = envelope_aad(header.subject_id.as_bytes(), header.key_version);
    cipher
        .decrypt(blob, &aad)
        .map_err(|e| CryptoShredError::Crypto(e.to_string()))
}

/// Seal a [`PropertyValue`] by encoding it to bytes then sealing.
///
/// # Errors
/// [`CryptoShredError::Crypto`] on encode failure, or the errors of [`seal`].
pub fn seal_property_value(
    value: &PropertyValue,
    subject_id: &str,
    key_version: u32,
    cipher: &dyn Cipher,
) -> Result<Vec<u8>, CryptoShredError> {
    let bytes = value
        .serialize()
        .map_err(|e| CryptoShredError::Crypto(format!("property encode failed: {e}")))?;
    seal(&bytes, subject_id, key_version, cipher)
}

/// Unseal an envelope back into a [`PropertyValue`].
///
/// # Errors
/// The errors of [`unseal`], plus [`CryptoShredError::Crypto`] if the decrypted
/// bytes do not decode to a `PropertyValue`.
pub fn unseal_property_value(
    envelope: &[u8],
    cipher: &dyn Cipher,
) -> Result<PropertyValue, CryptoShredError> {
    let bytes = unseal(envelope, cipher)?;
    let (value, _) = PropertyValue::deserialize(&bytes)
        .map_err(|e| CryptoShredError::Crypto(format!("property decode failed: {e}")))?;
    Ok(value)
}
