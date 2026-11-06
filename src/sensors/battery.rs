use jni::objects::JObject;
use jni::{jni_sig, jni_str, Env, JValue};

const BATTERY_PROPERTY_CAPACITY: i32 = 4;
/// `BatteryManager.BATTERY_PROPERTY_STATUS` (API 26) and its values.
const BATTERY_PROPERTY_STATUS: i32 = 6;
const BATTERY_STATUS_CHARGING: i32 = 2;
const BATTERY_STATUS_FULL: i32 = 5;

/// The charge, whether a supply is plugged in, and whether the battery charges.
/// A plugged supply can still lose to the load: on 2026-10-27 a 5 V 2.5 A
/// charger held the status at discharging with the hub and three cameras on.
pub fn read(env: &mut Env, activity: &JObject) -> jni::errors::Result<(i64, bool, bool)> {
    let service = env.new_string("batterymanager")?;
    let manager = env
        .call_method(
            activity,
            jni_str!("getSystemService"),
            jni_sig!("(Ljava/lang/String;)Ljava/lang/Object;"),
            &[JValue::Object(&service)],
        )?
        .l()?;
    let pct = env
        .call_method(
            &manager,
            jni_str!("getIntProperty"),
            jni_sig!("(I)I"),
            &[JValue::Int(BATTERY_PROPERTY_CAPACITY)],
        )?
        .i()?;
    // Not `isCharging()`: after a replug it stays false until the level has
    // risen 1%, which took 30 s on 2026-10-26 while the status said charging.
    let status = env
        .call_method(
            &manager,
            jni_str!("getIntProperty"),
            jni_sig!("(I)I"),
            &[JValue::Int(BATTERY_PROPERTY_STATUS)],
        )?
        .i()?;
    let charging = matches!(status, BATTERY_STATUS_CHARGING | BATTERY_STATUS_FULL);
    Ok((pct as i64, plugged(env, activity)?, charging))
}

fn plugged(env: &mut Env, activity: &JObject) -> jni::errors::Result<bool> {
    let sticky = battery_changed(env, activity)?;
    let extra = env.new_string("plugged")?;
    let plugged = env
        .call_method(
            &sticky,
            jni_str!("getIntExtra"),
            jni_sig!("(Ljava/lang/String;I)I"),
            &[JValue::Object(&extra), JValue::Int(0)],
        )?
        .i()?;
    Ok(plugged > 0)
}

/// `BatteryManager` property ids (API 21).
const BATTERY_PROPERTY_CHARGE_COUNTER: i32 = 1;
const BATTERY_PROPERTY_CURRENT_NOW: i32 = 2;

/// The headset's power and heat for `/diagnostics`: what the battery reports,
/// the sticky `ACTION_BATTERY_CHANGED` extras, and `PowerManager`'s thermal
/// status (API 29) and headroom (API 30). A value that cannot be read is left out.
pub fn health(env: &mut Env, activity: &JObject) -> crate::misc::status::Status {
    let mut status = crate::misc::status::Status::new("headset/power");
    let mut put = |key: &str, v: jni::errors::Result<String>| {
        if let Ok(v) = v {
            status.values.push((key.to_string(), v));
        }
    };
    let manager = system_service(env, activity, "batterymanager");
    let property = |env: &mut Env, id: i32| -> jni::errors::Result<String> {
        let manager =
            manager.as_ref().map_err(|_| jni::errors::Error::NullPtr("batterymanager"))?;
        let v = env
            .call_method(manager, jni_str!("getIntProperty"), jni_sig!("(I)I"), &[JValue::Int(id)])?
            .i()?;
        Ok(v.to_string())
    };
    put("capacity_pct", property(env, BATTERY_PROPERTY_CAPACITY));
    put("current_now_ua", property(env, BATTERY_PROPERTY_CURRENT_NOW));
    put("charge_counter_uah", property(env, BATTERY_PROPERTY_CHARGE_COUNTER));
    let sticky = battery_changed(env, activity);
    for (key, extra) in [
        ("temperature_dc", "temperature"),
        ("voltage_mv", "voltage"),
        ("plugged", "plugged"),
        ("status", "status"),
        ("health", "health"),
    ] {
        let v = sticky.as_ref().map_err(|_| jni::errors::Error::NullPtr("sticky")).and_then(|i| {
            let extra = env.new_string(extra)?;
            let v = env
                .call_method(
                    i,
                    jni_str!("getIntExtra"),
                    jni_sig!("(Ljava/lang/String;I)I"),
                    &[JValue::Object(&extra), JValue::Int(-1)],
                )?
                .i()?;
            Ok(v.to_string())
        });
        put(key, v);
    }
    if let Ok(power) = system_service(env, activity, "power") {
        let thermal =
            env.call_method(&power, jni_str!("getCurrentThermalStatus"), jni_sig!("()I"), &[]);
        put("thermal_status", thermal.and_then(|v| v.i()).map(|v| v.to_string()));
        let headroom = env.call_method(
            &power,
            jni_str!("getThermalHeadroom"),
            jni_sig!("(I)F"),
            &[JValue::Int(0)],
        );
        put("thermal_headroom", headroom.and_then(|v| v.f()).map(|v| v.to_string()));
    }
    env.exception_clear();
    status
}

fn system_service<'a>(
    env: &mut Env<'a>,
    activity: &JObject,
    name: &str,
) -> jni::errors::Result<JObject<'a>> {
    let service = env.new_string(name)?;
    env.call_method(
        activity,
        jni_str!("getSystemService"),
        jni_sig!("(Ljava/lang/String;)Ljava/lang/Object;"),
        &[JValue::Object(&service)],
    )?
    .l()
}

/// `registerReceiver(null, …)` hands back the last sticky broadcast without
/// registering anything.
fn battery_changed<'a>(env: &mut Env<'a>, activity: &JObject) -> jni::errors::Result<JObject<'a>> {
    let action = env.new_string("android.intent.action.BATTERY_CHANGED")?;
    let filter = env.new_object(
        jni_str!("android/content/IntentFilter"),
        jni_sig!("(Ljava/lang/String;)V"),
        &[JValue::Object(&action)],
    )?;
    let none = JObject::null();
    env.call_method(
        activity,
        jni_str!("registerReceiver"),
        jni_sig!(
            "(Landroid/content/BroadcastReceiver;Landroid/content/IntentFilter;)Landroid/content/Intent;"
        ),
        &[JValue::Object(&none), JValue::Object(&filter)],
    )?
    .l()
}

/// The battery reports whole percent, and a Quest 3 collecting moves one in
/// about a minute, so reading faster re-reads the same number.
const PERIOD: std::time::Duration = std::time::Duration::from_secs(20);

/// Power and heat are read this often for `/diagnostics`; thermal throttling
/// is what this is for, and Android re-evaluates thermal status on the order
/// of seconds, so 5 s keeps up without a JNI round trip every tick.
const HEALTH_PERIOD: std::time::Duration = std::time::Duration::from_secs(5);

/// The slow chores, on one thread. Each is a JNI read or a file read that
/// costs a wakeup and nothing else. Anything that can block on the network
/// needs a thread of its own, because one timeout here stalls the rest.
pub fn watch(vm: *mut std::ffi::c_void, activity: *mut std::ffi::c_void) {
    let (vm, activity) = (vm as usize, activity as usize);
    let mut complained = false;
    sample(vm, activity, &mut complained);
    std::thread::spawn(move || {
        let mut battery_at = std::time::Instant::now();
        let mut health_at = std::time::Instant::now() - HEALTH_PERIOD;
        loop {
            if health_at.elapsed() >= HEALTH_PERIOD {
                health_at = std::time::Instant::now();
                let vm = unsafe { jni::JavaVM::from_raw(vm as *mut _) };
                let status = vm.attach_current_thread(|env| {
                    let activity = unsafe { JObject::from_raw(env, activity as _) };
                    Ok::<_, jni::errors::Error>(health(env, &activity))
                });
                if let Ok(status) = status {
                    crate::app::hub().set_status(status);
                }
            }
            watch_supply(vm, activity);
            if battery_at.elapsed() >= PERIOD {
                battery_at = std::time::Instant::now();
                sample(vm, activity, &mut complained);
            }
            std::thread::sleep(PLUG_PERIOD);
        }
    });
}

/// The plug is read at this rate; the battery every
/// [`PERIOD`]. A flapping supply stays plugged for ~1 s of every ~3.5 s, so a
/// 1 s read sees most of its changes.
const PLUG_PERIOD: std::time::Duration = std::time::Duration::from_secs(1);

/// Says when a charger is plugged in or out, and warns when it keeps dropping
/// out: each drop resets the hub, so every camera and encoder reconnects.
fn watch_supply(raw_vm: usize, activity: usize) {
    let vm = unsafe { jni::JavaVM::from_raw(raw_vm as *mut _) };
    let plugged = vm.attach_current_thread(|env| {
        let activity = unsafe { JObject::from_raw(env, activity as _) };
        plugged(env, &activity)
    });
    if let Ok(plugged) = plugged {
        if let Some(supply) = crate::misc::power::note_plugged(plugged) {
            match supply {
                crate::misc::power::Supply::Unstable => {
                    log::warn!("power supply keeps dropping out; the hub resets with it")
                }
                supply => log::info!("power supply: {supply:?}"),
            }
            // So B right after the announcement says the new charging state,
            // not the one up to [`PERIOD`] old.
            sample(raw_vm, activity, &mut false);
            crate::app::hub().say_supply(supply);
        }
    }
}

fn sample(vm: usize, activity: usize, complained: &mut bool) {
    let vm = unsafe { jni::JavaVM::from_raw(vm as *mut _) };
    let read = vm.attach_current_thread(|env| {
        let activity = unsafe { JObject::from_raw(env, activity as _) };
        read(env, &activity)
    });
    match read {
        Ok((pct, plugged, charging)) => crate::misc::power::note(pct, plugged, charging),
        Err(e) if !*complained => {
            log::warn!("battery unreadable: {e}");
            *complained = true;
        }
        Err(_) => {}
    }
}
