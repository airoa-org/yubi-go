use glam::Mat4;

#[derive(Default)]
pub struct Mesh {
    pub verts: Vec<f32>,
}

impl Mesh {
    pub fn triangles(&self) -> usize {
        self.verts.len() / 18
    }

    fn push(&mut self, p: [f32; 3], n: [f32; 3]) {
        self.verts.extend_from_slice(&p);
        self.verts.extend_from_slice(&n);
    }
}

pub fn parse_stl(bytes: &[u8]) -> Option<Mesh> {
    let count = u32::from_le_bytes(bytes.get(80..84)?.try_into().ok()?) as usize;
    if bytes.len() < 84 + count * 50 {
        return None;
    }
    let f32_at = |o: usize| f32::from_le_bytes(bytes[o..o + 4].try_into().unwrap());
    let vec_at = |o: usize| [f32_at(o), f32_at(o + 4), f32_at(o + 8)];

    let mut mesh = Mesh { verts: Vec::with_capacity(count * 18) };
    for i in 0..count {
        let tri = 84 + i * 50;
        let normal = vec_at(tri);
        for v in 0..3 {
            mesh.push(vec_at(tri + 12 + v * 12), normal);
        }
    }
    Some(mesh)
}

pub fn projection(angles: [f32; 4], near: f32, far: f32) -> Mat4 {
    let [left, right, up, down] = angles.map(f32::tan);
    let (width, height) = (right - left, up - down);
    Mat4::from_cols_array(&[
        2.0 / width,
        0.0,
        0.0,
        0.0,
        0.0,
        2.0 / height,
        0.0,
        0.0,
        (right + left) / width,
        (up + down) / height,
        -(far + near) / (far - near),
        -1.0,
        0.0,
        0.0,
        -2.0 * far * near / (far - near),
        0.0,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_shipped_finger_meshes() {
        for (name, bytes) in crate::misc::mount::MESHES {
            let mesh = parse_stl(bytes).unwrap_or_else(|| panic!("{name} failed to parse"));
            assert!(mesh.triangles() > 1000, "{name}: {} triangles", mesh.triangles());
            let max = mesh.verts.chunks(6).map(|v| v[0].abs()).fold(0.0f32, f32::max);
            assert!((0.05..0.5).contains(&max), "{name}: x extent {max} is not metres");
        }
    }

    #[test]
    fn rejects_truncated_and_ascii_stl() {
        assert!(parse_stl(b"solid whatever\nfacet normal 0 0 0\n").is_none());
        assert!(parse_stl(&[0u8; 40]).is_none());
        let mut truncated = vec![0u8; 84];
        truncated[80] = 10;
        assert!(parse_stl(&truncated).is_none());
    }

    #[test]
    fn projection_matches_a_symmetric_frustum() {
        let a = 45f32.to_radians();
        let m = projection([-a, a, a, -a], 0.1, 100.0);
        let near = m * glam::Vec4::new(0.0, 0.0, -0.1, 1.0);
        assert!((near.z / near.w + 1.0).abs() < 1e-4, "near plane maps to {}", near.z / near.w);
        assert!((m.col(0)[0] - 1.0 / a.tan()).abs() < 1e-5);
        assert_eq!(m.col(2)[3], -1.0, "must be a perspective divide");
    }

    #[test]
    fn asymmetric_fov_shears_the_frustum() {
        let m = projection([-0.9, 0.7, 0.8, -0.8], 0.1, 100.0);
        assert!(m.col(2)[0].abs() > 0.05, "no horizontal shear: {}", m.col(2)[0]);
        let sym = projection([-0.8, 0.8, 0.8, -0.8], 0.1, 100.0);
        assert!(sym.col(2)[0].abs() < 1e-6, "symmetric fov must not shear");
    }
}

#[cfg(target_os = "android")]
pub use gles::Renderer;

#[cfg(target_os = "android")]
mod gles {
    use super::Mesh;
    use glam::Mat4;
    use std::ffi::{c_char, c_void, CString};

    type GLuint = u32;
    #[link(name = "GLESv3")]
    extern "C" {
        fn glCreateShader(kind: u32) -> GLuint;
        fn glShaderSource(s: GLuint, n: i32, str: *const *const c_char, len: *const i32);
        fn glCompileShader(s: GLuint);
        fn glGetShaderiv(s: GLuint, pname: u32, out: *mut i32);
        fn glGetShaderInfoLog(s: GLuint, max: i32, len: *mut i32, log: *mut c_char);
        fn glCreateProgram() -> GLuint;
        fn glAttachShader(p: GLuint, s: GLuint);
        fn glLinkProgram(p: GLuint);
        fn glGetProgramiv(p: GLuint, pname: u32, out: *mut i32);
        fn glUseProgram(p: GLuint);
        fn glGenBuffers(n: i32, out: *mut GLuint);
        fn glBindBuffer(target: u32, buf: GLuint);
        fn glBufferData(target: u32, size: isize, data: *const c_void, usage: u32);
        fn glEnableVertexAttribArray(index: u32);
        fn glVertexAttribPointer(i: u32, n: i32, t: u32, norm: u8, stride: i32, p: *const c_void);
        fn glDrawArrays(mode: u32, first: i32, count: i32);
        fn glGetUniformLocation(p: GLuint, name: *const c_char) -> i32;
        fn glUniformMatrix4fv(loc: i32, n: i32, transpose: u8, v: *const f32);
        fn glUniform4f(loc: i32, x: f32, y: f32, z: f32, w: f32);
        fn glEnable(cap: u32);
        fn glDisable(cap: u32);
        fn glBlendFuncSeparate(src_rgb: u32, dst_rgb: u32, src_a: u32, dst_a: u32);
        fn glViewport(x: i32, y: i32, w: i32, h: i32);
    }

    const GL_ARRAY_BUFFER: u32 = 0x8892;
    const GL_STATIC_DRAW: u32 = 0x88E4;
    const GL_FLOAT: u32 = 0x1406;
    const GL_TRIANGLES: u32 = 0x0004;
    const GL_VERTEX_SHADER: u32 = 0x8B31;
    const GL_FRAGMENT_SHADER: u32 = 0x8B30;
    const GL_COMPILE_STATUS: u32 = 0x8B81;
    const GL_LINK_STATUS: u32 = 0x8B82;
    const GL_CULL_FACE: u32 = 0x0B44;
    const GL_ONE: u32 = 1;
    const GL_SRC_ALPHA: u32 = 0x0302;
    const GL_ONE_MINUS_SRC_ALPHA: u32 = 0x0303;
    const GL_BLEND: u32 = 0x0BE2;

    const VERTEX_SHADER: &str = "#version 300 es
layout(location = 0) in vec3 position;
layout(location = 1) in vec3 normal;
uniform mat4 mvp;
uniform mat4 model;
out vec3 world_normal;
void main() {
    world_normal = mat3(model) * normal;
    gl_Position = mvp * vec4(position, 1.0);
}
";

    const FRAGMENT_SHADER: &str = "#version 300 es
precision mediump float;
in vec3 world_normal;
uniform vec4 color;
out vec4 frag_color;
void main() {
    vec3 n = normalize(world_normal);
    float key = max(dot(n, normalize(vec3(0.3, 0.9, 0.4))), 0.0);
    frag_color = vec4(color.rgb * (0.45 + 0.55 * key), color.a);
}
";

    fn compile(kind: u32, source: &str) -> anyhow::Result<GLuint> {
        unsafe {
            let shader = glCreateShader(kind);
            let src = CString::new(source)?;
            glShaderSource(shader, 1, &src.as_ptr(), std::ptr::null());
            glCompileShader(shader);
            let mut ok = 0;
            glGetShaderiv(shader, GL_COMPILE_STATUS, &mut ok);
            if ok == 0 {
                let mut log = [0u8; 512];
                let mut len = 0;
                glGetShaderInfoLog(shader, 512, &mut len, log.as_mut_ptr() as *mut c_char);
                anyhow::bail!("shader: {}", String::from_utf8_lossy(&log[..len as usize]));
            }
            Ok(shader)
        }
    }

    pub struct Renderer {
        program: GLuint,
        mvp: i32,
        model: i32,
        color: i32,
        /// `(mesh name, buffer, vertex count)`.
        buffers: Vec<(&'static str, GLuint, i32)>,
    }

    impl Renderer {
        pub fn new(meshes: &[(&'static str, Mesh)]) -> anyhow::Result<Self> {
            unsafe {
                let program = glCreateProgram();
                glAttachShader(program, compile(GL_VERTEX_SHADER, VERTEX_SHADER)?);
                glAttachShader(program, compile(GL_FRAGMENT_SHADER, FRAGMENT_SHADER)?);
                glLinkProgram(program);
                let mut ok = 0;
                glGetProgramiv(program, GL_LINK_STATUS, &mut ok);
                anyhow::ensure!(ok != 0, "shader program failed to link");

                let uniform = |name: &str| -> anyhow::Result<i32> {
                    let c = CString::new(name)?;
                    Ok(glGetUniformLocation(program, c.as_ptr()))
                };
                let mut buffers = Vec::with_capacity(meshes.len());
                for &(name, ref mesh) in meshes {
                    let mut buffer = 0;
                    glGenBuffers(1, &mut buffer);
                    glBindBuffer(GL_ARRAY_BUFFER, buffer);
                    glBufferData(
                        GL_ARRAY_BUFFER,
                        std::mem::size_of_val(&mesh.verts[..]) as isize,
                        mesh.verts.as_ptr() as *const c_void,
                        GL_STATIC_DRAW,
                    );
                    buffers.push((name, buffer, (mesh.verts.len() / 6) as i32));
                }
                Ok(Self {
                    program,
                    mvp: uniform("mvp")?,
                    model: uniform("model")?,
                    color: uniform("color")?,
                    buffers,
                })
            }
        }

        pub fn draw(
            &self,
            w: u32,
            h: u32,
            view_proj: Mat4,
            items: &[(&str, Mat4)],
            rgba: [f32; 4],
        ) {
            unsafe {
                glViewport(0, 0, w as i32, h as i32);
                glEnable(GL_CULL_FACE);
                glEnable(GL_BLEND);
                glBlendFuncSeparate(
                    GL_SRC_ALPHA,
                    GL_ONE_MINUS_SRC_ALPHA,
                    GL_ONE,
                    GL_ONE_MINUS_SRC_ALPHA,
                );
                glUseProgram(self.program);
                glUniform4f(self.color, rgba[0], rgba[1], rgba[2], rgba[3]);
                for &(name, model) in items {
                    let Some(&(_, buffer, count)) = self.buffers.iter().find(|b| b.0 == name)
                    else {
                        continue;
                    };
                    glBindBuffer(GL_ARRAY_BUFFER, buffer);
                    let stride = 6 * std::mem::size_of::<f32>() as i32;
                    glEnableVertexAttribArray(0);
                    glVertexAttribPointer(0, 3, GL_FLOAT, 0, stride, std::ptr::null());
                    glEnableVertexAttribArray(1);
                    glVertexAttribPointer(1, 3, GL_FLOAT, 0, stride, (3 * 4) as *const c_void);
                    let mvp = view_proj * model;
                    glUniformMatrix4fv(self.mvp, 1, 0, mvp.to_cols_array().as_ptr());
                    glUniformMatrix4fv(self.model, 1, 0, model.to_cols_array().as_ptr());
                    glDrawArrays(GL_TRIANGLES, 0, count);
                }
                glDisable(GL_BLEND);
                glDisable(GL_CULL_FACE);
            }
        }
    }
}
