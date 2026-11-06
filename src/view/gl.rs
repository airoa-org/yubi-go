use std::ffi::c_void;

pub type Display = *mut c_void;
pub type Config = *mut c_void;
pub type Context = *mut c_void;
type Surface = *mut c_void;

const NONE: i32 = 0x3038;
const ALPHA_SIZE: i32 = 0x3021;
const BLUE_SIZE: i32 = 0x3022;
const GREEN_SIZE: i32 = 0x3023;
const RED_SIZE: i32 = 0x3024;
const SURFACE_TYPE: i32 = 0x3033;
const PBUFFER_BIT: i32 = 0x0001;
const WINDOW_BIT: i32 = 0x0004;
const RENDERABLE_TYPE: i32 = 0x3040;
const OPENGL_ES3_BIT: i32 = 0x0040;
const HEIGHT: i32 = 0x3056;
const WIDTH: i32 = 0x3057;
const CONTEXT_CLIENT_VERSION: i32 = 0x3098;

#[link(name = "EGL")]
extern "C" {
    fn eglGetDisplay(display_id: *mut c_void) -> Display;
    fn eglInitialize(dpy: Display, major: *mut i32, minor: *mut i32) -> u32;
    fn eglChooseConfig(
        dpy: Display,
        attrib_list: *const i32,
        configs: *mut Config,
        config_size: i32,
        num_config: *mut i32,
    ) -> u32;
    fn eglCreatePbufferSurface(dpy: Display, config: Config, attrib_list: *const i32) -> Surface;
    fn eglCreateContext(
        dpy: Display,
        config: Config,
        share: Context,
        attrib_list: *const i32,
    ) -> Context;
    fn eglMakeCurrent(dpy: Display, draw: Surface, read: Surface, ctx: Context) -> u32;
}

pub struct Egl {
    pub display: Display,
    pub config: Config,
    pub context: Context,
}

impl Egl {
    pub fn new() -> anyhow::Result<Self> {
        unsafe {
            let display = eglGetDisplay(std::ptr::null_mut());
            if display.is_null() {
                anyhow::bail!("eglGetDisplay failed");
            }
            if eglInitialize(display, std::ptr::null_mut(), std::ptr::null_mut()) == 0 {
                anyhow::bail!("eglInitialize failed");
            }

            let config_attrs = [
                RENDERABLE_TYPE,
                OPENGL_ES3_BIT,
                SURFACE_TYPE,
                PBUFFER_BIT | WINDOW_BIT,
                RED_SIZE,
                8,
                GREEN_SIZE,
                8,
                BLUE_SIZE,
                8,
                ALPHA_SIZE,
                8,
                NONE,
            ];
            let mut config: Config = std::ptr::null_mut();
            let mut count = 0;
            if eglChooseConfig(display, config_attrs.as_ptr(), &mut config, 1, &mut count) == 0
                || count == 0
            {
                anyhow::bail!("eglChooseConfig found no config");
            }

            let surface_attrs = [WIDTH, 1, HEIGHT, 1, NONE];
            let surface = eglCreatePbufferSurface(display, config, surface_attrs.as_ptr());
            if surface.is_null() {
                anyhow::bail!("eglCreatePbufferSurface failed");
            }

            let context_attrs = [CONTEXT_CLIENT_VERSION, 3, NONE];
            let context =
                eglCreateContext(display, config, std::ptr::null_mut(), context_attrs.as_ptr());
            if context.is_null() {
                anyhow::bail!("eglCreateContext failed");
            }
            if eglMakeCurrent(display, surface, surface, context) == 0 {
                anyhow::bail!("eglMakeCurrent failed");
            }

            Ok(Egl { display, config, context })
        }
    }
}

pub const RGBA8: u32 = 0x8058;

const FRAMEBUFFER: u32 = 0x8D40;
const COLOR_ATTACHMENT0: u32 = 0x8CE0;
const TEXTURE_2D: u32 = 0x0DE1;
const COLOR_BUFFER_BIT: u32 = 0x4000;
const RGBA: u32 = 0x1908;
const UNSIGNED_BYTE: u32 = 0x1401;
const UNPACK_ROW_LENGTH: u32 = 0x0CF2;

#[link(name = "GLESv3")]
extern "C" {
    fn glGenFramebuffers(n: i32, out: *mut u32);
    fn glBindFramebuffer(target: u32, fb: u32);
    fn glFramebufferTexture2D(
        target: u32,
        attachment: u32,
        textarget: u32,
        texture: u32,
        level: i32,
    );
    fn glClearColor(r: f32, g: f32, b: f32, a: f32);
    fn glClear(mask: u32);
    fn glBindTexture(target: u32, texture: u32);
    fn glPixelStorei(pname: u32, param: i32);
    fn glTexSubImage2D(
        target: u32,
        level: i32,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        format: u32,
        kind: u32,
        pixels: *const c_void,
    );
}

/// Colour only: the grippers are drawn translucent, which writes no depth.
pub fn framebuffer_for(texture: u32) -> u32 {
    unsafe {
        let mut fbo = 0;
        glGenFramebuffers(1, &mut fbo);
        glBindFramebuffer(FRAMEBUFFER, fbo);
        glFramebufferTexture2D(FRAMEBUFFER, COLOR_ATTACHMENT0, TEXTURE_2D, texture, 0);
        fbo
    }
}

pub fn bind_and_clear(fbo: u32) {
    unsafe {
        glBindFramebuffer(FRAMEBUFFER, fbo);
        glClearColor(0.0, 0.0, 0.0, 0.0);
        glClear(COLOR_BUFFER_BIT);
    }
}

pub fn unbind() {
    unsafe { glBindFramebuffer(FRAMEBUFFER, 0) }
}

/// Upload the `w`x`h` rect at (`x`, `y`) of a `stride`-wide RGBA buffer,
/// rows bottom-up as GL counts them.
pub fn upload_rect(texture: u32, stride: usize, pixels: &[u8], (x, y, w, h): Rect) {
    let at = (y * stride + x) * 4;
    assert!(h > 0 && pixels.len() >= at + ((h - 1) * stride + w) * 4);
    unsafe {
        glBindTexture(TEXTURE_2D, texture);
        glPixelStorei(UNPACK_ROW_LENGTH, stride as i32);
        glTexSubImage2D(
            TEXTURE_2D,
            0,
            x as i32,
            y as i32,
            w as i32,
            h as i32,
            RGBA,
            UNSIGNED_BYTE,
            pixels[at..].as_ptr() as *const _,
        );
        glPixelStorei(UNPACK_ROW_LENGTH, 0);
        glBindTexture(TEXTURE_2D, 0);
    }
}

type Rect = (usize, usize, usize, usize);
