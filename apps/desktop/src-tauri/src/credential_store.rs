//! Native credential storage. Never persist API keys in workspace JSON or web storage.
//! This module intentionally has no plaintext-disk fallback.
#[cfg(target_os = "linux")]
mod platform {
    use std::io::Write;
    use std::process::{Command, Stdio};
    pub fn set(provider: &str, key: &str) -> Result<(), String> {
        let mut child = Command::new("secret-tool")
            .args(["store", "--label=Ryoko model provider", "application", "dev.ryoko.desktop", "provider", provider])
            .stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null())
            .spawn().map_err(|_| "GNOME Keyring / libsecret (secret-tool) is required for encrypted credential storage".to_owned())?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(key.as_bytes())
                .map_err(|_| "could not send credential to system keyring".to_owned())?;
        }
        if child.wait().map_err(|error| error.to_string())?.success() {
            Ok(())
        } else {
            Err("System keyring rejected the credential. Check that the secret service is unlocked.".to_owned())
        }
    }
    pub fn get(provider: &str) -> Result<Option<String>, String> {
        let result = Command::new("secret-tool")
            .args([
                "lookup",
                "application",
                "dev.ryoko.desktop",
                "provider",
                provider,
            ])
            .output()
            .map_err(|_| "GNOME Keyring / libsecret (secret-tool) is unavailable".to_owned())?;
        if !result.status.success() {
            return Ok(None);
        }
        let value = String::from_utf8(result.stdout)
            .map_err(|_| "keyring returned invalid text".to_owned())?;
        let value = value.trim_end_matches(['\r', '\n']);
        Ok((!value.is_empty()).then(|| value.to_owned()))
    }
}
#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::c_void;
    use std::ptr;
    #[link(name = "Security", kind = "framework")]
    unsafe extern "C" {
        fn SecKeychainFindGenericPassword(
            chain: *const c_void,
            service_len: u32,
            service: *const c_void,
            account_len: u32,
            account: *const c_void,
            password_len: *mut u32,
            password: *mut *mut c_void,
            item: *mut *mut c_void,
        ) -> i32;
        fn SecKeychainAddGenericPassword(
            chain: *const c_void,
            service_len: u32,
            service: *const c_void,
            account_len: u32,
            account: *const c_void,
            password_len: u32,
            password: *const c_void,
            item: *mut *mut c_void,
        ) -> i32;
        fn SecKeychainItemModifyAttributesAndData(
            item: *mut c_void,
            attributes: *const c_void,
            password_len: u32,
            password: *const c_void,
        ) -> i32;
        fn SecKeychainItemFreeContent(attributes: *const c_void, data: *mut c_void) -> i32;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(item: *const c_void);
    }
    const SERVICE: &[u8] = b"dev.ryoko.desktop";
    const NOT_FOUND: i32 = -25300;
    pub fn set(provider: &str, key: &str) -> Result<(), String> {
        let mut item = ptr::null_mut();
        let lookup = unsafe {
            SecKeychainFindGenericPassword(
                ptr::null(),
                SERVICE.len() as u32,
                SERVICE.as_ptr().cast(),
                provider.len() as u32,
                provider.as_ptr().cast(),
                ptr::null_mut(),
                ptr::null_mut(),
                &mut item,
            )
        };
        let status = if lookup == 0 {
            let result = unsafe {
                SecKeychainItemModifyAttributesAndData(
                    item,
                    ptr::null(),
                    key.len() as u32,
                    key.as_ptr().cast(),
                )
            };
            unsafe {
                CFRelease(item);
            }
            result
        } else if lookup == NOT_FOUND {
            unsafe {
                SecKeychainAddGenericPassword(
                    ptr::null(),
                    SERVICE.len() as u32,
                    SERVICE.as_ptr().cast(),
                    provider.len() as u32,
                    provider.as_ptr().cast(),
                    key.len() as u32,
                    key.as_ptr().cast(),
                    ptr::null_mut(),
                )
            }
        } else {
            lookup
        };
        if status == 0 {
            Ok(())
        } else {
            Err(format!(
                "macOS Keychain refused the credential (status {status})"
            ))
        }
    }
    pub fn get(provider: &str) -> Result<Option<String>, String> {
        let mut len = 0u32;
        let mut data = ptr::null_mut();
        let status = unsafe {
            SecKeychainFindGenericPassword(
                ptr::null(),
                SERVICE.len() as u32,
                SERVICE.as_ptr().cast(),
                provider.len() as u32,
                provider.as_ptr().cast(),
                &mut len,
                &mut data,
                ptr::null_mut(),
            )
        };
        if status == NOT_FOUND {
            return Ok(None);
        }
        if status != 0 {
            return Err(format!("macOS Keychain lookup failed (status {status})"));
        }
        let value = unsafe {
            String::from_utf8(std::slice::from_raw_parts(data.cast::<u8>(), len as usize).to_vec())
        };
        unsafe {
            SecKeychainItemFreeContent(ptr::null(), data);
        }
        value
            .map(Some)
            .map_err(|_| "keychain returned invalid text".to_owned())
    }
}
#[cfg(target_os = "windows")]
mod platform {
    use std::ffi::c_void;
    use std::ptr;
    #[repr(C)]
    struct FileTime {
        low: u32,
        high: u32,
    }
    #[repr(C)]
    struct Credential {
        flags: u32,
        credential_type: u32,
        target_name: *mut u16,
        comment: *mut u16,
        last_written: FileTime,
        blob_size: u32,
        blob: *mut u8,
        persist: u32,
        attribute_count: u32,
        attributes: *mut c_void,
        target_alias: *mut u16,
        user_name: *mut u16,
    }
    #[link(name = "Advapi32")]
    unsafe extern "system" {
        fn CredWriteW(credential: *const Credential, flags: u32) -> i32;
        fn CredReadW(
            target: *const u16,
            kind: u32,
            flags: u32,
            result: *mut *mut Credential,
        ) -> i32;
        fn CredFree(buffer: *mut c_void);
    }
    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain([0]).collect()
    }
    fn target(provider: &str) -> Vec<u16> {
        wide(&format!("Ryoko/dev.ryoko.desktop/{provider}"))
    }
    pub fn set(provider: &str, key: &str) -> Result<(), String> {
        let mut target = target(provider);
        let mut username = wide("Ryoko");
        let mut bytes = key.as_bytes().to_vec();
        let credential = Credential {
            flags: 0,
            credential_type: 1,
            target_name: target.as_mut_ptr(),
            comment: ptr::null_mut(),
            last_written: FileTime { low: 0, high: 0 },
            blob_size: bytes.len() as u32,
            blob: bytes.as_mut_ptr(),
            persist: 2,
            attribute_count: 0,
            attributes: ptr::null_mut(),
            target_alias: ptr::null_mut(),
            user_name: username.as_mut_ptr(),
        };
        if unsafe { CredWriteW(&credential, 0) } != 0 {
            Ok(())
        } else {
            Err("Windows Credential Manager could not store the key".to_owned())
        }
    }
    pub fn get(provider: &str) -> Result<Option<String>, String> {
        let target = target(provider);
        let mut result: *mut Credential = ptr::null_mut();
        if unsafe { CredReadW(target.as_ptr(), 1, 0, &mut result) } == 0 {
            return Ok(None);
        }
        if result.is_null() {
            return Ok(None);
        }
        let raw = unsafe { &*result };
        let value = unsafe {
            String::from_utf8(std::slice::from_raw_parts(raw.blob, raw.blob_size as usize).to_vec())
        };
        unsafe { CredFree(result.cast()) };
        value
            .map(Some)
            .map_err(|_| "Credential Manager returned invalid text".to_owned())
    }
}
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod platform {
    pub fn set(_: &str, _: &str) -> Result<(), String> {
        Err("No OS keychain adapter for this platform".to_owned())
    }
    pub fn get(_: &str) -> Result<Option<String>, String> {
        Err("No OS keychain adapter for this platform".to_owned())
    }
}
pub fn store(provider: &str, key: &str) -> Result<(), String> {
    platform::set(provider, key)
}
pub fn retrieve(provider: &str) -> Result<Option<String>, String> {
    platform::get(provider)
}
