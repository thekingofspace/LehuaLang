use std::io::{Read, Write};

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use flate2::read::{DeflateDecoder, MultiGzDecoder, ZlibDecoder};
use flate2::write::{DeflateEncoder, GzEncoder, ZlibEncoder};
use flate2::Compression;
use hmac::{Hmac, Mac};
use md5::Md5;
use mlua::Value;
use sha1::Sha1;
use sha2::{Digest, Sha224, Sha256, Sha384, Sha512};

use super::LibCtx;
use crate::error::LehuaError;

const NONCE_LEN: usize = 12;

pub fn build(ctx: &LibCtx) -> mlua::Result<Value> {
    let lua = ctx.lua;
    let t = lua.create_table()?;

    t.set(
        "hash",
        lua.create_function(|_, (algo, data): (String, mlua::LuaString)| {
            let bytes = hash_bytes(&algo, &data.as_bytes())?;
            Ok(hex::encode(bytes))
        })?,
    )?;

    t.set(
        "hmac",
        lua.create_function(
            |_, (algo, key, data): (String, mlua::LuaString, mlua::LuaString)| {
                let bytes = hmac_bytes(&algo, &key.as_bytes(), &data.as_bytes())?;
                Ok(hex::encode(bytes))
            },
        )?,
    )?;

    t.set(
        "crc32",
        lua.create_function(|_, data: mlua::LuaString| {
            let mut h = crc32fast::Hasher::new();
            h.update(&data.as_bytes());
            Ok(h.finalize())
        })?,
    )?;

    t.set(
        "compress",
        lua.create_function(
            |lua, (data, format, level): (mlua::LuaString, Option<String>, Option<u32>)| {
                let level = Compression::new(level.unwrap_or(6).min(9));
                let out = compress(&data.as_bytes(), format.as_deref().unwrap_or("gzip"), level)?;
                lua.create_string(out)
            },
        )?,
    )?;

    t.set(
        "decompress",
        lua.create_function(|lua, (data, format): (mlua::LuaString, Option<String>)| {
            let out = decompress(&data.as_bytes(), format.as_deref().unwrap_or("gzip"))?;
            lua.create_string(out)
        })?,
    )?;

    t.set(
        "base64Encode",
        lua.create_function(|_, data: mlua::LuaString| {
            use base64::Engine;
            Ok(base64::engine::general_purpose::STANDARD.encode(&data.as_bytes()[..]))
        })?,
    )?;

    t.set(
        "base64Decode",
        lua.create_function(|lua, text: String| {
            use base64::Engine;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(text.trim())
                .map_err(mlua::Error::external)?;
            lua.create_string(bytes)
        })?,
    )?;

    t.set(
        "hexEncode",
        lua.create_function(|_, data: mlua::LuaString| Ok(hex::encode(&data.as_bytes()[..])))?,
    )?;

    t.set(
        "hexDecode",
        lua.create_function(|lua, text: String| {
            let bytes = hex::decode(text.trim()).map_err(mlua::Error::external)?;
            lua.create_string(bytes)
        })?,
    )?;

    t.set(
        "encrypt",
        lua.create_function(|lua, (key, plaintext): (mlua::LuaString, mlua::LuaString)| {
            let cipher = make_cipher(&key.as_bytes());
            let mut nonce = [0u8; NONCE_LEN];
            getrandom::fill(&mut nonce)
                .map_err(|e| LehuaError::msg(format!("random source failed: {e}")))?;
            let sealed = cipher
                .encrypt(&Nonce::try_from(&nonce[..]).unwrap(), &plaintext.as_bytes()[..])
                .map_err(|_| LehuaError::msg("encryption failed"))?;
            let mut out = Vec::with_capacity(NONCE_LEN + sealed.len());
            out.extend_from_slice(&nonce);
            out.extend_from_slice(&sealed);
            lua.create_string(out)
        })?,
    )?;

    t.set(
        "decrypt",
        lua.create_function(|lua, (key, data): (mlua::LuaString, mlua::LuaString)| {
            let data = data.as_bytes();
            if data.len() <= NONCE_LEN {
                return Err(LehuaError::msg("decrypt: data is too short").into());
            }
            let cipher = make_cipher(&key.as_bytes());
            let nonce = Nonce::try_from(&data[..NONCE_LEN])
                .map_err(|_| LehuaError::msg("decrypt: data is too short"))?;
            let plain = cipher
                .decrypt(&nonce, &data[NONCE_LEN..])
                .map_err(|_| {
                    LehuaError::msg("decrypt failed: wrong key or corrupted data")
                })?;
            lua.create_string(plain)
        })?,
    )?;

    t.set(
        "passwordHash",
        lua.create_function(|_, password: mlua::LuaString| {
            use argon2::password_hash::{PasswordHasher, SaltString};
            let mut salt_bytes = [0u8; 16];
            getrandom::fill(&mut salt_bytes)
                .map_err(|e| LehuaError::msg(format!("passwordHash failed: {e}")))?;
            let salt = SaltString::encode_b64(&salt_bytes)
                .map_err(|e| LehuaError::msg(format!("passwordHash failed: {e}")))?;
            let hash = argon2::Argon2::default()
                .hash_password(&password.as_bytes(), &salt)
                .map_err(|e| LehuaError::msg(format!("passwordHash failed: {e}")))?;
            Ok(hash.to_string())
        })?,
    )?;

    t.set(
        "passwordVerify",
        lua.create_function(|_, (password, hash): (mlua::LuaString, String)| {
            use argon2::password_hash::{PasswordHash, PasswordVerifier};
            let parsed = PasswordHash::new(&hash)
                .map_err(|e| LehuaError::msg(format!("invalid password hash: {e}")))?;
            Ok(argon2::Argon2::default()
                .verify_password(&password.as_bytes(), &parsed)
                .is_ok())
        })?,
    )?;

    t.set(
        "randomBytes",
        lua.create_function(|lua, n: usize| {
            let mut buf = Vec::new();
            buf.try_reserve_exact(n)
                .map_err(|_| LehuaError::msg(format!("randomBytes: {n} bytes is too much")))?;
            buf.resize(n, 0);
            getrandom::fill(&mut buf)
                .map_err(|e| LehuaError::msg(format!("random source failed: {e}")))?;
            lua.create_string(buf)
        })?,
    )?;

    t.set(
        "uuid",
        lua.create_function(|_, ()| Ok(uuid::Uuid::new_v4().to_string()))?,
    )?;

    Ok(Value::Table(t))
}

fn hash_bytes(algo: &str, data: &[u8]) -> mlua::Result<Vec<u8>> {
    let out = match algo.trim().to_ascii_lowercase().as_str() {
        "md5" => Md5::digest(data).to_vec(),
        "sha1" => Sha1::digest(data).to_vec(),
        "sha224" => Sha224::digest(data).to_vec(),
        "sha256" => Sha256::digest(data).to_vec(),
        "sha384" => Sha384::digest(data).to_vec(),
        "sha512" => Sha512::digest(data).to_vec(),
        other => {
            return Err(LehuaError::msg(format!(
                "unknown hash algorithm '{other}' (supported: md5, sha1, sha224, sha256, sha384, sha512)"
            ))
            .into())
        }
    };
    Ok(out)
}

fn hmac_bytes(algo: &str, key: &[u8], data: &[u8]) -> mlua::Result<Vec<u8>> {
    macro_rules! mac {
        ($h:ty) => {{
            let mut m = <Hmac<$h> as hmac::KeyInit>::new_from_slice(key)
                .map_err(|_| LehuaError::msg("hmac: invalid key"))?;
            m.update(data);
            m.finalize().into_bytes().to_vec()
        }};
    }
    let out = match algo.trim().to_ascii_lowercase().as_str() {
        "md5" => mac!(Md5),
        "sha1" => mac!(Sha1),
        "sha256" => mac!(Sha256),
        "sha384" => mac!(Sha384),
        "sha512" => mac!(Sha512),
        other => {
            return Err(LehuaError::msg(format!(
                "unknown hmac algorithm '{other}' (supported: md5, sha1, sha256, sha384, sha512)"
            ))
            .into())
        }
    };
    Ok(out)
}

fn make_cipher(key: &[u8]) -> Aes256Gcm {
    let key = Sha256::digest(key);
    Aes256Gcm::new(&key)
}

fn compress(data: &[u8], format: &str, level: Compression) -> mlua::Result<Vec<u8>> {
    let out = match format.trim().to_ascii_lowercase().as_str() {
        "gzip" | "gz" => {
            let mut enc = GzEncoder::new(Vec::new(), level);
            enc.write_all(data).map_err(mlua::Error::external)?;
            enc.finish().map_err(mlua::Error::external)?
        }
        "zlib" => {
            let mut enc = ZlibEncoder::new(Vec::new(), level);
            enc.write_all(data).map_err(mlua::Error::external)?;
            enc.finish().map_err(mlua::Error::external)?
        }
        "deflate" => {
            let mut enc = DeflateEncoder::new(Vec::new(), level);
            enc.write_all(data).map_err(mlua::Error::external)?;
            enc.finish().map_err(mlua::Error::external)?
        }
        other => return Err(bad_compression(other)),
    };
    Ok(out)
}

fn decompress(data: &[u8], format: &str) -> mlua::Result<Vec<u8>> {
    let mut out = Vec::new();
    match format.trim().to_ascii_lowercase().as_str() {
        "gzip" | "gz" => MultiGzDecoder::new(data)
            .read_to_end(&mut out)
            .map_err(mlua::Error::external)?,
        "zlib" => ZlibDecoder::new(data)
            .read_to_end(&mut out)
            .map_err(mlua::Error::external)?,
        "deflate" => DeflateDecoder::new(data)
            .read_to_end(&mut out)
            .map_err(mlua::Error::external)?,
        other => return Err(bad_compression(other)),
    };
    Ok(out)
}

fn bad_compression(format: &str) -> mlua::Error {
    LehuaError::msg(format!(
        "unknown compression format '{format}' (supported: gzip, zlib, deflate)"
    ))
    .into()
}
