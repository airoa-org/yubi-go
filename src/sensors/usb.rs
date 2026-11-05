use jni::objects::{JObject, JString};
use jni::{jni_sig, jni_str, Env, JValue};
use log::{info, warn};
use std::os::fd::{FromRawFd, OwnedFd};

pub use crate::misc::usb::Kind;

pub struct UsbCamera {
    pub name: String,
    fd: Option<OwnedFd>,
    pub kind: Kind,
    /// The hub port (`1-1.2.6`), when sysfs would say.
    pub chain: Option<String>,
    /// `/dev/bus/usb/BBB/DDD`.
    pub path: String,
    pub conn: Option<jni::objects::Global<jni::objects::JObject<'static>>>,
}

static VM: std::sync::OnceLock<jni::JavaVM> = std::sync::OnceLock::new();

pub fn remember_vm(vm: jni::JavaVM) {
    let _ = VM.set(vm);
}

impl UsbCamera {
    pub fn take_fd(&mut self) -> Option<OwnedFd> {
        self.fd.take()
    }

    /// The hub port this device is plugged into (`misc::usb::port_name`), from
    /// `USBDEVFS_CONNINFO_EX` (Linux 5.5; the Quest 3S runs 5.10). `None` once
    /// the fd is taken, or on a kernel without the ioctl.
    pub fn port(&self) -> Option<String> {
        #[repr(C)]
        #[derive(Default)]
        struct ConnInfoEx {
            size: u32,
            busnum: u32,
            devnum: u32,
            speed: u32,
            num_ports: u8,
            ports: [u8; 7],
        }
        // _IOC(_IOC_READ, 'U', 32, sizeof(struct usbdevfs_conninfo_ex))
        const USBDEVFS_CONNINFO_EX: u32 = (2 << 30)
            | ((std::mem::size_of::<ConnInfoEx>() as u32) << 16)
            | ((b'U' as u32) << 8)
            | 32;
        use std::os::fd::AsRawFd;
        let fd = self.fd.as_ref()?.as_raw_fd();
        let mut info = ConnInfoEx::default();
        // SAFETY: the kernel writes at most the size encoded in the request
        // number, which is the size of `info`.
        let rc = unsafe { libc::ioctl(fd, USBDEVFS_CONNINFO_EX as _, &mut info) };
        if rc < 0 {
            return None;
        }
        let n = (info.num_ports as usize).min(info.ports.len());
        Some(crate::misc::usb::port_name(info.busnum, &info.ports[..n]))
    }
}

impl Drop for UsbCamera {
    fn drop(&mut self) {
        let Some(conn) = self.conn.take() else { return };
        let Some(vm) = VM.get() else { return };
        let _ = vm.attach_current_thread(|env| -> jni::errors::Result<()> {
            env.call_method(&conn, jni_str!("close"), jni_sig!("()V"), &[])?;
            drop(conn);
            Ok(())
        });
    }
}

/// Horizon OS hands a USB video-class device to an app only when it holds both
/// CAMERA and `horizonos.permission.USB_CAMERA`; with CAMERA alone it logs
/// `USB Camera permission required for USB video class devices`.
pub fn ensure_camera_permission(
    env: &mut Env,
    activity: &JObject,
    ask: bool,
) -> jni::errors::Result<bool> {
    let mut missing = Vec::new();
    for name in ["android.permission.CAMERA", "horizonos.permission.USB_CAMERA"] {
        let perm = env.new_string(name)?;
        let granted = env
            .call_method(
                activity,
                jni_str!("checkSelfPermission"),
                jni_sig!("(Ljava/lang/String;)I"),
                &[JValue::Object(&perm)],
            )?
            .i()?
            == 0;
        if !granted {
            missing.push(perm);
        }
    }
    if !missing.is_empty() && ask {
        info!("requesting camera runtime permissions ({} missing)", missing.len());
        let perms = jni::objects::JObjectArray::<JString>::new(env, missing.len(), &missing[0])?;
        for (i, perm) in missing.iter().enumerate().skip(1) {
            perms.set_element(env, i, perm)?;
        }
        env.call_method(
            activity,
            jni_str!("requestPermissions"),
            jni_sig!("([Ljava/lang/String;I)V"),
            &[JValue::Object(&perms), JValue::Int(1)],
        )?;
    }
    Ok(missing.is_empty())
}

fn get_usb_manager<'l>(env: &mut Env<'l>, activity: &JObject) -> jni::errors::Result<JObject<'l>> {
    let service_name = env.new_string("usb")?;
    env.call_method(
        activity,
        jni_str!("getSystemService"),
        jni_sig!("(Ljava/lang/String;)Ljava/lang/Object;"),
        &[JValue::Object(&service_name)],
    )?
    .l()
}

fn for_each_device(
    env: &mut Env,
    mgr: &JObject,
    mut f: impl FnMut(&mut Env, &JObject) -> jni::errors::Result<()>,
) -> jni::errors::Result<()> {
    let map = env
        .call_method(mgr, jni_str!("getDeviceList"), jni_sig!("()Ljava/util/HashMap;"), &[])?
        .l()?;
    let values = env
        .call_method(&map, jni_str!("values"), jni_sig!("()Ljava/util/Collection;"), &[])?
        .l()?;
    let iter = env
        .call_method(&values, jni_str!("iterator"), jni_sig!("()Ljava/util/Iterator;"), &[])?
        .l()?;
    let mut devices = Vec::new();
    while env.call_method(&iter, jni_str!("hasNext"), jni_sig!("()Z"), &[])?.z()? {
        let dev =
            env.call_method(&iter, jni_str!("next"), jni_sig!("()Ljava/lang/Object;"), &[])?.l()?;
        let name = device_name(env, &dev);
        devices.push((name, dev));
    }
    devices.sort_by(|a, b| a.0.cmp(&b.0));
    for (_, dev) in &devices {
        f(env, dev)?;
    }
    Ok(())
}

fn classify(env: &mut Env, dev: &JObject) -> jni::errors::Result<Option<Kind>> {
    let n = env.call_method(dev, jni_str!("getInterfaceCount"), jni_sig!("()I"), &[])?.i()?;
    let mut interfaces = Vec::with_capacity(n as usize);
    for i in 0..n {
        let intf = env
            .call_method(
                dev,
                jni_str!("getInterface"),
                jni_sig!("(I)Landroid/hardware/usb/UsbInterface;"),
                &[JValue::Int(i)],
            )?
            .l()?;
        let class =
            env.call_method(&intf, jni_str!("getInterfaceClass"), jni_sig!("()I"), &[])?.i()?;
        let subclass =
            env.call_method(&intf, jni_str!("getInterfaceSubclass"), jni_sig!("()I"), &[])?.i()?;
        interfaces.push((class as u8, subclass as u8));
    }
    Ok(crate::misc::usb::classify(interfaces))
}

fn device_name(env: &mut Env, dev: &JObject) -> String {
    let mut get = |name, sig| -> Option<String> {
        let s = env.call_method(dev, name, sig, &[]).ok()?.l().ok()?;
        if s.is_null() {
            return None;
        }
        JString::cast_local(env, s).ok()?.try_to_string(env).ok()
    };
    let product =
        get(jni_str!("getProductName"), jni_sig!("()Ljava/lang/String;")).unwrap_or_default();
    let path = get(jni_str!("getDeviceName"), jni_sig!("()Ljava/lang/String;")).unwrap_or_default();
    format!("{product} ({path})")
}

/// What the descriptors say about a device we just opened — the only place
/// a serial number or a vendor id exists, and meta.json's `devices` wants
/// both. Anything Android will not answer is left empty, never guessed.
fn describe(
    env: &mut Env,
    dev: &JObject,
    kind: Kind,
    port: Option<String>,
) -> crate::misc::usb::UsbDevice {
    let mut text = |name, sig| -> String {
        let value = env.call_method(dev, name, sig, &[]).map(|v| v.l());
        // getSerialNumber throws without permission on Android 10+; a
        // pending exception poisons every later JNI call on this thread.
        if env.exception_check() {
            env.exception_clear();
            return String::new();
        }
        match value {
            Ok(Ok(s)) if !s.is_null() => JString::cast_local(env, s)
                .ok()
                .and_then(|s| s.try_to_string(env).ok())
                .unwrap_or_default(),
            _ => String::new(),
        }
    };
    let product = text(jni_str!("getProductName"), jni_sig!("()Ljava/lang/String;"));
    let manufacturer = text(jni_str!("getManufacturerName"), jni_sig!("()Ljava/lang/String;"));
    let serial = text(jni_str!("getSerialNumber"), jni_sig!("()Ljava/lang/String;"));
    let path = device_path(env, dev);
    let mut int = |name| env.call_method(dev, name, jni_sig!("()I"), &[]).and_then(|v| v.i());
    let ids = match (int(jni_str!("getVendorId")), int(jni_str!("getProductId"))) {
        (Ok(vid), Ok(pid)) => format!("{vid:04x}:{pid:04x}"),
        _ => String::new(),
    };
    crate::misc::usb::UsbDevice {
        name: crate::misc::usb::descriptor_text(&product),
        vendor: crate::misc::usb::descriptor_text(&manufacturer),
        model: ids,
        serial: crate::misc::usb::descriptor_text(&serial),
        interface: match kind {
            Kind::Video => "usb-uvc".into(),
            Kind::Serial => "usb-cdc".into(),
        },
        path,
        port: port.unwrap_or_default(),
    }
}

fn has_permission(env: &mut Env, mgr: &JObject, dev: &JObject) -> jni::errors::Result<bool> {
    env.call_method(
        mgr,
        jni_str!("hasPermission"),
        jni_sig!("(Landroid/hardware/usb/UsbDevice;)Z"),
        &[JValue::Object(dev)],
    )?
    .z()
}

fn request_permission(
    env: &mut Env,
    activity: &JObject,
    mgr: &JObject,
    dev: &JObject,
) -> jni::errors::Result<()> {
    let action = env.new_string("org.airoa.yubigo.USB_PERMISSION")?;
    let intent_class = env.find_class(jni_str!("android/content/Intent"))?;
    let intent = env.new_object(
        intent_class,
        jni_sig!("(Ljava/lang/String;)V"),
        &[JValue::Object(&action)],
    )?;
    let pkg = env
        .call_method(activity, jni_str!("getPackageName"), jni_sig!("()Ljava/lang/String;"), &[])?
        .l()?;
    env.call_method(
        &intent,
        jni_str!("setPackage"),
        jni_sig!("(Ljava/lang/String;)Landroid/content/Intent;"),
        &[JValue::Object(&pkg)],
    )?;
    const FLAG_MUTABLE: i32 = 0x0200_0000;
    let pi_class = env.find_class(jni_str!("android/app/PendingIntent"))?;
    let pi = env
        .call_static_method(
            pi_class,
            jni_str!("getBroadcast"),
            jni_sig!(
                "(Landroid/content/Context;ILandroid/content/Intent;I)Landroid/app/PendingIntent;"
            ),
            &[
                JValue::Object(activity),
                JValue::Int(0),
                JValue::Object(&intent),
                JValue::Int(FLAG_MUTABLE),
            ],
        )?
        .l()?;
    env.call_method(
        mgr,
        jni_str!("requestPermission"),
        jni_sig!("(Landroid/hardware/usb/UsbDevice;Landroid/app/PendingIntent;)V"),
        &[JValue::Object(dev), JValue::Object(&pi)],
    )?;
    Ok(())
}

fn open_device(
    env: &mut Env,
    mgr: &JObject,
    dev: &JObject,
) -> jni::errors::Result<Option<(OwnedFd, jni::objects::Global<jni::objects::JObject<'static>>)>> {
    let conn = env
        .call_method(
            mgr,
            jni_str!("openDevice"),
            jni_sig!(
                "(Landroid/hardware/usb/UsbDevice;)Landroid/hardware/usb/UsbDeviceConnection;"
            ),
            &[JValue::Object(dev)],
        )?
        .l()?;
    if conn.is_null() {
        return Ok(None);
    }
    let fd = env.call_method(&conn, jni_str!("getFileDescriptor"), jni_sig!("()I"), &[])?.i()?;
    let keep_alive = env.new_global_ref(&conn)?;
    let dup = unsafe { libc::dup(fd) };
    if dup < 0 {
        warn!("dup({fd}) failed");
        let _ = env.call_method(&conn, jni_str!("close"), jni_sig!("()V"), &[]);
        return Ok(None);
    }
    Ok(Some((unsafe { OwnedFd::from_raw_fd(dup) }, keep_alive)))
}

pub fn scan(
    env: &mut Env,
    activity: &JObject,
    skip: &std::collections::HashSet<String>,
    requested: &mut std::collections::HashSet<String>,
) -> jni::errors::Result<(Vec<UsbCamera>, usize)> {
    let mgr = get_usb_manager(env, activity)?;
    let mut cams = Vec::new();
    let mut pending = 0;
    let mut paths = Vec::new();

    for_each_device(env, &mgr, |env, dev| {
        paths.push(device_path(env, dev));
        let Some(kind) = classify(env, dev)? else {
            return Ok(());
        };
        let name = device_name(env, dev);
        if skip.contains(&name) {
            return Ok(());
        }
        if !has_permission(env, &mgr, dev)? {
            if requested.insert(name.clone()) {
                info!("requesting USB permission for {name}");
                request_permission(env, activity, &mgr, dev)?;
            }
            pending += 1;
            return Ok(());
        }
        match open_device(env, &mgr, dev)? {
            Some((fd, conn)) => {
                let path = device_path(env, dev);
                let chain = crate::misc::usb::port_chain(&path);
                let opened = format!(
                    "opened {name} ({kind:?}) at hub port {}",
                    chain.as_deref().unwrap_or("?")
                );
                info!("{opened}");
                crate::misc::usb::event(&crate::app::hub().dir(), &opened);
                let cam = UsbCamera { name, fd: Some(fd), kind, chain, path, conn: Some(conn) };
                let port = cam.port().or_else(|| cam.chain.clone());
                crate::misc::usb::note(describe(env, dev, kind, port));
                cams.push(cam);
            }
            None => warn!("openDevice failed for {name}"),
        }
        Ok(())
    })?;
    crate::misc::usb::retain(&paths);
    Ok((cams, pending))
}

/// `/dev/bus/usb/BBB/DDD` — every attached device has one, permission or not.
fn device_path(env: &mut Env, dev: &JObject) -> String {
    let mut get = || -> Option<String> {
        let s = env
            .call_method(dev, jni_str!("getDeviceName"), jni_sig!("()Ljava/lang/String;"), &[])
            .ok()?
            .l()
            .ok()?;
        if s.is_null() {
            return None;
        }
        JString::cast_local(env, s).ok()?.try_to_string(env).ok()
    };
    get().unwrap_or_default()
}
