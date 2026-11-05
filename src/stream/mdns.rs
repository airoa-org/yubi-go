//! The headset's name on the LAN, `yubi-go-075.local`, advertised by the app
//! itself so that a page opens without anyone looking up an IP.

use std::sync::OnceLock;

/// The `.local` host, once it is advertised.
static SHOWN: OnceLock<String> = OnceLock::new();

pub fn shown() -> Option<&'static str> {
    SHOWN.get().map(String::as_str)
}

/// A bare `yubi-go` is refused because it is `robot_id`'s default, so every
/// unnamed headset on one network would claim the same host.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn label(name: &str) -> anyhow::Result<String> {
    let name = name.trim();
    let label =
        if name.starts_with("yubi-go-") { name.to_owned() } else { format!("yubi-go-{name}") };
    anyhow::ensure!(
        !name.is_empty()
            && name != "yubi-go"
            && label.len() <= 63
            && !label.ends_with('-')
            && label.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-'),
        "{name:?} is not a unique DNS label (letters, digits, hyphens; max 63 bytes)"
    );
    Ok(label)
}

/// Advertises `name` on `port` until the process exits. The daemon is never
/// shut down: the name ages out of other hosts' caches once it stops answering.
#[cfg(target_os = "android")]
fn advertise(name: &str, port: u16) -> anyhow::Result<()> {
    use mdns_sd::{ServiceDaemon, ServiceInfo};

    let label = label(name)?;
    let host = format!("{label}.local.");
    let service =
        ServiceInfo::new("_http._tcp.local.", &label, &host, "", port, &[("path", "/")][..])?
            .enable_addr_auto();
    let daemon = ServiceDaemon::new()?;
    daemon.set_ip_check_interval(5)?;
    daemon.register(service)?;
    std::mem::forget(daemon);
    let _ = SHOWN.set(format!("{label}.local"));
    Ok(())
}

/// Android drops inbound multicast without a `MulticastLock`, so no query
/// would ever reach the daemon. The lock is held until the process exits,
/// because `MulticastLock` releases itself when it is collected.
#[cfg(target_os = "android")]
pub fn start(
    vm: *mut std::ffi::c_void,
    activity: *mut std::ffi::c_void,
    name: &str,
    port: u16,
) -> anyhow::Result<()> {
    use jni::objects::JObject;
    use jni::{jni_sig, jni_str, JValue};

    label(name)?;
    let vm = unsafe { jni::JavaVM::from_raw(vm as *mut _) };
    vm.attach_current_thread(|env| -> jni::errors::Result<()> {
        let activity = unsafe { JObject::from_raw(env, activity as _) };
        let wifi = env.new_string("wifi")?;
        let manager = env
            .call_method(
                &activity,
                jni_str!("getSystemService"),
                jni_sig!("(Ljava/lang/String;)Ljava/lang/Object;"),
                &[JValue::Object(&wifi)],
            )?
            .l()?;
        let tag = env.new_string("yubi-go-mdns")?;
        let lock = env
            .call_method(
                &manager,
                jni_str!("createMulticastLock"),
                jni_sig!("(Ljava/lang/String;)Landroid/net/wifi/WifiManager$MulticastLock;"),
                &[JValue::Object(&tag)],
            )?
            .l()?;
        env.call_method(&lock, jni_str!("acquire"), jni_sig!("()V"), &[])?;
        std::mem::forget(env.new_global_ref(&lock)?);
        Ok(())
    })?;
    advertise(name, port)
}

#[cfg(test)]
mod tests {
    use super::label;

    #[test]
    fn the_label_is_the_name_under_one_yubi_go_prefix() {
        assert_eq!(label("075").unwrap(), "yubi-go-075");
        assert_eq!(label("yubi-go-075").unwrap(), "yubi-go-075");
        for bad in ["", " ", "yubi-go", "quest-", "quest.local", "a b", "端末", &"a".repeat(56)] {
            assert!(label(bad).is_err(), "{bad}");
        }
    }
}
