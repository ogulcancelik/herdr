//! Shared framing and small utilities for the client and server sides of remote QUIC.

use std::sync::{Mutex, MutexGuard};

use serde::{de::DeserializeOwned, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::protocol::{MAX_GRAPHICS_FRAME_SIZE, REMOTE_QUIC_HASH_BYTES};

/// Allocation limit for decoding untrusted payloads. The outer frame is already
/// capped by `max`, but bincode would otherwise trust an attacker-declared
/// internal String/Vec length and pre-allocate it before auth. bincode 2's
/// limit is a const generic, so we use the largest frame cap any caller passes
/// (`MAX_GRAPHICS_FRAME_SIZE`); declared lengths beyond it fail instead of
/// allocating.
/// ponytail: a per-call runtime limit isn't expressible with bincode 2's const
/// generic API; if bincode grows a runtime limit, tie it to `max` directly.
const DECODE_LIMIT: usize = MAX_GRAPHICS_FRAME_SIZE;

pub(crate) async fn write_async_message<M>(
    stream: &mut (impl AsyncWrite + Unpin),
    message: &M,
    max: usize,
) -> Result<(), String>
where
    M: Serialize,
{
    let payload = bincode::serde::encode_to_vec(message, bincode::config::standard())
        .map_err(|err| format!("failed to encode QUIC message: {err}"))?;
    if payload.len() > max || payload.len() > u32::MAX as usize {
        return Err(format!(
            "QUIC message size {} exceeds maximum {max}",
            payload.len()
        ));
    }
    stream
        .write_all(&(payload.len() as u32).to_le_bytes())
        .await
        .map_err(|err| format!("failed to write QUIC message length: {err}"))?;
    stream
        .write_all(&payload)
        .await
        .map_err(|err| format!("failed to write QUIC message: {err}"))
}

pub(crate) async fn read_async_message<M>(
    stream: &mut (impl AsyncRead + Unpin),
    max: usize,
) -> Result<M, String>
where
    M: DeserializeOwned,
{
    let mut length = [0u8; 4];
    stream
        .read_exact(&mut length)
        .await
        .map_err(|err| format!("failed to read QUIC message length: {err}"))?;
    let length = u32::from_le_bytes(length) as usize;
    if length > max {
        return Err(format!("QUIC message size {length} exceeds maximum {max}"));
    }
    let mut payload = vec![0u8; length];
    stream
        .read_exact(&mut payload)
        .await
        .map_err(|err| format!("failed to read QUIC message: {err}"))?;
    let (message, consumed) = bincode::serde::decode_from_slice(
        &payload,
        bincode::config::standard().with_limit::<DECODE_LIMIT>(),
    )
    .map_err(|err| format!("failed to decode QUIC message: {err}"))?;
    if consumed != length {
        return Err("QUIC message contains trailing bytes".to_owned());
    }
    Ok(message)
}

pub(crate) fn hash_bytes(bytes: &[u8]) -> [u8; REMOTE_QUIC_HASH_BYTES] {
    Sha256::digest(bytes).into()
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn async_framing_round_trips_over_partial_io() {
        let (mut writer, mut reader) = tokio::io::duplex(7);
        let write = tokio::spawn(async move {
            write_async_message(&mut writer, &vec![1u16, 2, 3, 4], 128).await
        });
        let decoded: Vec<u16> = read_async_message(&mut reader, 128)
            .await
            .expect("decode framed message");
        write.await.expect("writer task").expect("encode message");
        assert_eq!(decoded, vec![1, 2, 3, 4]);
    }

    #[tokio::test]
    async fn async_framing_rejects_oversize_and_trailing_bytes() {
        let (mut writer, _reader) = tokio::io::duplex(32);
        let error = write_async_message(&mut writer, &vec![0u8; 16], 4)
            .await
            .expect_err("oversized message must fail");
        assert!(error.contains("exceeds maximum"));

        let (mut writer, mut reader) = tokio::io::duplex(32);
        let mut payload = bincode::serde::encode_to_vec(7u8, bincode::config::standard())
            .expect("encode fixture");
        payload.push(0xff);
        writer
            .write_all(&(payload.len() as u32).to_le_bytes())
            .await
            .expect("write fixture length");
        writer.write_all(&payload).await.expect("write fixture");
        let error = read_async_message::<u8>(&mut reader, 32)
            .await
            .expect_err("trailing bytes must fail");
        assert!(error.contains("trailing bytes"));
    }

    #[tokio::test]
    async fn decode_rejects_huge_declared_internal_length() {
        // A tiny frame whose bincode varint declares a multi-GiB string length
        // must fail decoding instead of pre-allocating that much memory.
        let (mut writer, mut reader) = tokio::io::duplex(64);
        // 0xfd marks a u64 varint length in bincode's standard config.
        let mut payload = vec![0xfdu8];
        payload.extend_from_slice(&(8u64 * 1024 * 1024 * 1024).to_le_bytes());
        writer
            .write_all(&(payload.len() as u32).to_le_bytes())
            .await
            .expect("write fixture length");
        writer.write_all(&payload).await.expect("write fixture");
        let error = read_async_message::<String>(&mut reader, 64)
            .await
            .expect_err("huge declared length must fail");
        assert!(error.contains("failed to decode"), "{error}");
    }
}
