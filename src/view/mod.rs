#[cfg(any(target_os = "android", test))]
mod episodes;
#[cfg(target_os = "android")]
mod gl;
#[cfg(any(target_os = "android", test))]
mod guide;
#[cfg(any(target_os = "android", test))]
pub mod mesh;
#[cfg(target_os = "android")]
mod painter;
#[cfg(any(target_os = "android", test))]
mod pairing;
#[cfg(any(target_os = "android", test))]
pub mod panel;
#[cfg(any(target_os = "android", test))]
pub mod place;
#[cfg(target_os = "android")]
mod run;
#[cfg(any(target_os = "android", test))]
mod sound;
#[cfg(any(target_os = "android", test))]
mod voice;
#[cfg(target_os = "android")]
mod xr;

#[cfg(target_os = "android")]
pub use run::{set_foreground, start};

/// Played first thing at launch, so someone not wearing the headset hears the
/// app come up, for example when it starts on a hub plug-in.
#[cfg(target_os = "android")]
pub fn play_boot() {
    sound::play_boot();
}

/// Said from the battery watcher, which runs outside the render loop.
#[cfg(target_os = "android")]
pub fn say_supply(supply: crate::misc::power::Supply) {
    sound::say(None, vec![voice::supply(supply)]);
}
