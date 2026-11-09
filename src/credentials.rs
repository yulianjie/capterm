use std::sync::Mutex;

use anyhow::{Context, Result, anyhow};
use keyring_core::{Entry, Error};
use sha2::{Digest, Sha256};

use crate::config::ConnectionProfile;

#[cfg(any(windows, target_os = "macos", target_os = "linux"))]
use keyring_core::api::CredentialStoreApi;

const SERVICE: &str = "CapTerm SSH";

// Some platform stores read or search for the entry before writing it.
static STORE_ACCESS: Mutex<()> = Mutex::new(());

pub fn load(profile: &ConnectionProfile) -> Result<Option<String>> {
    with_entry(profile, |entry| match entry.get_password() {
        Ok(password) => Ok(Some(password)),
        Err(Error::NoEntry) => Ok(None),
        Err(error) => Err(error),
    })
    .context("无法读取已保存的 SSH 密码")
}

pub fn save(profile: &ConnectionProfile, password: &str) -> Result<()> {
    with_entry(profile, |entry| entry.set_password(password)).context("无法保存 SSH 密码")
}

pub fn remove(profile: &ConnectionProfile) -> Result<()> {
    with_entry(profile, |entry| match entry.delete_credential() {
        Ok(()) | Err(Error::NoEntry) => Ok(()),
        Err(error) => Err(error),
    })
    .context("无法移除已保存的 SSH 密码")
}

pub fn same_target(first: &ConnectionProfile, second: &ConnectionProfile) -> bool {
    target(first) == target(second)
}

fn target(profile: &ConnectionProfile) -> ([&str; 3], u16) {
    ([&profile.id, &profile.host, &profile.user], profile.port)
}

fn entry_key(profile: &ConnectionProfile) -> String {
    let (fields, port) = target(profile);
    let mut hash = Sha256::new();
    for field in fields {
        hash.update((field.len() as u64).to_be_bytes());
        hash.update(field.as_bytes());
    }
    hash.update(port.to_be_bytes());
    hex::encode(hash.finalize())
}

fn with_entry<T>(
    profile: &ConnectionProfile,
    operation: impl FnOnce(Entry) -> keyring_core::Result<T>,
) -> Result<T> {
    let _guard = STORE_ACCESS
        .lock()
        .map_err(|_| anyhow!("系统凭据存储锁不可用"))?;
    let entry = native_entry(&entry_key(profile)).map_err(safe_error)?;
    operation(entry).map_err(safe_error)
}

fn safe_error(error: Error) -> anyhow::Error {
    // Some error variants carry raw credential bytes in their Debug representation.
    match error {
        Error::BadEncoding(_) | Error::BadDataFormat(..) => anyhow!("系统凭据的密码格式无效"),
        other => anyhow!("{other}"),
    }
}

#[cfg(windows)]
fn native_entry(key: &str) -> keyring_core::Result<Entry> {
    let target = format!("{SERVICE}/{key}");
    let options = [("target", target.as_str()), ("persistence", "local")].into();
    windows_native_keyring_store::Store::new()?.build(SERVICE, key, Some(&options))
}

#[cfg(target_os = "macos")]
fn native_entry(key: &str) -> keyring_core::Result<Entry> {
    apple_native_keyring_store::keychain::Store::new()?.build(SERVICE, key, None)
}

#[cfg(target_os = "linux")]
fn native_entry(key: &str) -> keyring_core::Result<Entry> {
    let options = [("label", SERVICE)].into();
    zbus_secret_service_keyring_store::Store::new()?.build(SERVICE, key, Some(&options))
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn native_entry(_: &str) -> keyring_core::Result<Entry> {
    Err(Error::NotSupportedByStore(
        "当前平台没有可用的系统凭据存储".into(),
    ))
}
