//! Shaders over part of a window, with the OpenGL renderer (Settings › System › Renderer):
//! the Terminal's screen, and the Library's preview.
//!
//! The format is Ghostty's `custom-shader` (which is Shadertoy's): a `mainImage(out vec4,
//! in vec2)` that reads the area from `iChannel0`. It gets Shadertoy's `iResolution`,
//! `iTime`, `iTimeDelta`, `iFrame`, `iDate`, `iChannelResolution` and `iMouse` (always
//! zero: the pointer is not tracked), and Ghostty's own: the cursor (`iCurrentCursor`,
//! `iPreviousCursor`, their colors, `iTimeCursorChange`), the focus (`iFocus`,
//! `iTimeFocus`) and the colors (`iBackgroundColor`, `iForegroundColor`, `iCursorColor`,
//! `iPalette[256]`). Several shaders form a chain, each reading the one before.
//!
//! MiniUtils adds inputs of its own: `iTimeError` (when the shell last rang the bell or
//! reported a failed command, on the `iTime` clock; far in the past when it never did),
//! `iChannel1` (what this same shader drew the frame before, for trails that fade),
//! `iMoodColor` (the theme's color for the mood of the window's mascot) and `iSeed` (a
//! number of the tab on screen: each new tab gets the next one).
//!
//! After Slint draws the window (`RenderingState::AfterRendering`), the area is copied into
//! a texture and drawn back in place through the chain. Every GL state touched is restored,
//! since the renderer's own drawing relies on it. A shader that reads the clock (`iTime`,
//! …) makes the window draw on every frame while it is visible; the others only when
//! something changes, like the rest of the window. A shader whose animation only follows
//! events says so with `#pragma miniutils events`: the window then draws on every frame
//! only for `EVENT_TAIL` after the cursor moves, the focus changes or an error comes. One
//! that draws a background under the text says `#pragma miniutils background`
//! (`Source::paints_background`).
//!
//! Compile errors are kept by shader name (`errors`), for the Library to show.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use glow::HasContext;
use slint::{ComponentHandle, GraphicsAPI, RenderingState, Timer, TimerMode};

/// How often an animated chain asks for a frame (the display's rate caps it anyway).
const FRAME: Duration = Duration::from_millis(16);
/// How long a chain that animates on events keeps drawing after the last one.
const EVENT_TAIL: Duration = Duration::from_millis(2500);
/// The line that makes a shader animate on events only (see the module's comment).
const EVENTS_PRAGMA: &str = "#pragma miniutils events";
/// The line of a shader that paints the background (see the module's comment).
const BACKGROUND_PRAGMA: &str = "#pragma miniutils background";

/// GLSL headers tried, newest first. 4.30 takes what Ghostty shaders use (array
/// initializers); 3.30 gets the same through an extension, for a driver that gives exactly
/// the 3.3 context asked for (NVIDIA); ES 3.00 is the context Slint falls back to when
/// there is no desktop OpenGL 3.3.
const VERSIONS: [&str; 3] = [
    "#version 430 core\n",
    "#version 330 core\n#extension GL_ARB_shading_language_420pack : enable\n",
    "#version 300 es\nprecision highp float;\nprecision highp int;\n",
];

/// A full-window triangle, from the vertex index alone (no buffer).
const VERTEX: &str = "
void main() {
    vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
    gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}
";

/// What a Ghostty shader expects to find declared, and `main` calling its `mainImage` with
/// the pixel's position inside the area (`uOffset`: where the area starts).
const PRELUDE: &str = "
uniform vec3 iResolution;
uniform float iTime;
uniform float iTimeDelta;
uniform int iFrame;
uniform vec4 iMouse;
uniform vec4 iDate;
uniform sampler2D iChannel0;
uniform sampler2D iChannel1;
uniform float iTimeError;
uniform vec3 iMoodColor;
uniform float iSeed;
uniform vec3 iChannelResolution[4];
uniform vec4 iCurrentCursor;
uniform vec4 iPreviousCursor;
uniform vec4 iCurrentCursorColor;
uniform vec4 iPreviousCursorColor;
uniform float iTimeCursorChange;
uniform int iFocus;
uniform float iTimeFocus;
uniform vec3 iBackgroundColor;
uniform vec3 iForegroundColor;
uniform vec3 iCursorColor;
uniform vec3 iPalette[256];
uniform vec2 uOffset;
out vec4 miniutilsColor;
";

const MAIN: &str = "
void main() {
    vec4 color = vec4(0.0);
    mainImage(color, gl_FragCoord.xy - uOffset);
    miniutilsColor = color;
}
";

/// One shader of the chain: its name (the Library key, for errors and the log), the GLSL
/// to compile (with the user's adjustments already written in) and the values of its
/// choices, which it declares as `uniform float NAME;` (see `shaders::source`).
#[derive(Clone, PartialEq)]
pub struct Source {
    pub name: String,
    pub glsl: String,
    pub uniforms: Vec<(String, f32)>,
}

impl Source {
    /// It draws a background (the text stays on top): it goes first in the chain, and the
    /// Terminal paints a plain background under it instead of the skin's.
    pub fn paints_background(&self) -> bool {
        without_comments(&self.glsl).contains(BACKGROUND_PRAGMA)
    }

    /// How the window must draw for it: a shader that reads the clock changes on its own
    /// (on every frame, or after events when it says so). Comments don't count (a shader
    /// may say it doesn't use `iTime`).
    fn redraw(&self) -> Redraw {
        let code = without_comments(&self.glsl);
        let clock = ["iTime", "iTimeDelta", "iFrame", "iDate", "iTimeCursorChange", "iTimeFocus", "iTimeError", "iChannel1"];
        if !clock.iter().any(|u| code.contains(u)) {
            Redraw::Still
        } else if code.contains(EVENTS_PRAGMA) {
            Redraw::Events
        } else {
            Redraw::Always
        }
    }
}

/// When a chain needs frames of its own, besides the window's.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
enum Redraw {
    /// Never: it only changes with the window.
    #[default]
    Still,
    /// For `EVENT_TAIL` after each event.
    Events,
    /// On every frame.
    Always,
}

/// GLSL without its `//` and `/* */` comments.
fn without_comments(glsl: &str) -> String {
    let mut out = String::with_capacity(glsl.len());
    let mut rest = glsl;
    while let Some(i) = rest.find('/') {
        out.push_str(&rest[..i]);
        let after = &rest[i..];
        rest = if after.starts_with("//") {
            after.find('\n').map_or("", |n| &after[n..])
        } else if let Some(body) = after.strip_prefix("/*") {
            body.find("*/").map_or("", |n| &body[n + 2..])
        } else {
            out.push('/');
            &after[1..]
        };
    }
    out.push_str(rest);
    out
}

/// What the window shows this frame, read by the probe given to `attach`.
pub struct Frame {
    /// The area: x, y, width and height in logical pixels from the window's top left.
    pub area: (f32, f32, f32, f32),
    /// The text cursor, if one is shown: x, y, width and height in logical pixels from the
    /// area's top left.
    pub cursor: Option<(f32, f32, f32, f32)>,
    pub focused: bool,
}

/// The colors a shader can ask for, as 0..1 RGB.
#[derive(Clone)]
pub struct Colors {
    pub background: [f32; 3],
    pub foreground: [f32; 3],
    pub cursor: [f32; 3],
    pub palette: Box<[[f32; 3]; 256]>,
}

impl Colors {
    /// From the 16 terminal colors; the other 240 are xterm's cube and grays.
    pub fn new(background: [f32; 3], foreground: [f32; 3], cursor: [f32; 3], ansi: [[f32; 3]; 16]) -> Self {
        let mut palette = Box::new([[0.0; 3]; 256]);
        palette[..16].copy_from_slice(&ansi);
        let level = |i: usize| if i == 0 { 0.0 } else { (55 + 40 * i) as f32 / 255.0 };
        for i in 0..216 {
            palette[16 + i] = [level(i / 36), level(i / 6 % 6), level(i % 6)];
        }
        for i in 0..24 {
            let v = (8 + 10 * i) as f32 / 255.0;
            palette[232 + i] = [v, v, v];
        }
        Colors { background, foreground, cursor, palette }
    }
}

thread_local! {
    /// The last compile error of each shader, by name (none = it compiled).
    static ERRORS: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
    /// Bumped whenever `ERRORS` changes, so a screen can tell without comparing.
    static ERRORS_STAMP: Cell<u64> = const { Cell::new(0) };
}

/// Why shader `name` did not compile the last time it was built (UI thread).
pub fn error(name: &str) -> Option<String> {
    ERRORS.with(|e| e.borrow().get(name).cloned())
}

/// Changes whenever an error appears or goes away.
pub fn errors_stamp() -> u64 {
    ERRORS_STAMP.with(Cell::get)
}

fn set_error(name: &str, error: Option<String>) {
    let changed = ERRORS.with(|e| {
        let mut e = e.borrow_mut();
        match error {
            Some(msg) => e.insert(name.to_string(), msg.clone()).as_ref() != Some(&msg),
            None => e.remove(name).is_some(),
        }
    });
    if changed {
        ERRORS_STAMP.with(|s| s.set(s.get() + 1));
    }
}

/// The chain asked for, the colors, and whether the GL side has built it yet.
#[derive(Default)]
struct Shared {
    sources: Vec<Source>,
    changed: bool,
    colors: Option<Colors>,
    /// What the chain needs to keep moving, and the last event that set it going.
    redraw: Redraw,
    kicked: Option<Instant>,
    /// When the shell last failed (`iTimeError`).
    error: Option<Instant>,
    /// The mascot's mood color (`iMoodColor`).
    mood: [f32; 3],
    /// The tab's number (`iSeed`).
    seed: f32,
}

impl Shared {
    /// The clock has frames to ask for now.
    fn moving(&self) -> bool {
        match self.redraw {
            Redraw::Still => false,
            Redraw::Events => self.kicked.is_some_and(|at| at.elapsed() < EVENT_TAIL),
            Redraw::Always => true,
        }
    }
}

/// The shaders of one window.
pub struct PostFx {
    shared: Rc<RefCell<Shared>>,
    /// Asks for frames while the chain is moving (`Shared::moving`).
    clock: Rc<Timer>,
}

impl PostFx {
    /// Hooks into the window's drawing; `probe` says, each frame, where the area is (`None`:
    /// nothing to draw over now). `None` with the software renderer (no OpenGL).
    pub fn attach<C: ComponentHandle + 'static>(win: &C, probe: impl Fn(&C) -> Option<Frame> + 'static) -> Option<PostFx> {
        if crate::popup::renderer() != crate::popup::Renderer::OpenGl {
            return None;
        }
        let shared = Rc::new(RefCell::new(Shared::default()));
        let clock = Rc::new(Timer::default());
        let (inner, ticker) = (shared.clone(), clock.clone());
        let weak = win.as_weak();
        let mut gl: Option<Gl> = None;
        let hooked = win.window().set_rendering_notifier(move |state, api| match state {
            RenderingState::RenderingSetup => {
                if let GraphicsAPI::NativeOpenGL { get_proc_address } = api {
                    // SAFETY: the renderer's context is current during the notifier, and the
                    // loader is the one it was created with.
                    let context = unsafe { glow::Context::from_loader_function_cstr(|name| get_proc_address(name)) };
                    gl = Some(Gl::new(context));
                    // a new context: the programs are built again in it
                    inner.borrow_mut().changed = true;
                }
            }
            RenderingState::AfterRendering => {
                if let (Some(gl), Some(win)) = (gl.as_mut(), weak.upgrade())
                    && let Some(frame) = probe(&win)
                {
                    let size = win.window().size();
                    let event = gl.frame(&frame, win.window().scale_factor(), (size.width as i32, size.height as i32), &mut inner.borrow_mut());
                    if event {
                        inner.borrow_mut().kicked = Some(Instant::now());
                        run_clock(&ticker, weak.clone(), &inner);
                    }
                }
            }
            RenderingState::RenderingTeardown => {
                if let Some(gl) = gl.take() {
                    gl.destroy();
                }
            }
            _ => {}
        });
        if let Err(e) = hooked {
            log::warn!("no shaders in this window ({e:?})");
            return None;
        }
        Some(PostFx { shared, clock })
    }

    /// The shaders to run, in order (empty: none). Draws the window again when they changed.
    pub fn set<C: ComponentHandle + 'static>(&self, win: &C, sources: Vec<Source>) {
        {
            let mut shared = self.shared.borrow_mut();
            shared.redraw = sources.iter().map(Source::redraw).max_by_key(|r| *r as u8).unwrap_or_default();
            if shared.sources != sources {
                // only the choices changed: the same programs, new values
                let same_code = shared.sources.len() == sources.len()
                    && shared.sources.iter().zip(&sources).all(|(a, b)| a.name == b.name && a.glsl == b.glsl);
                shared.changed |= !same_code;
                shared.sources = sources;
                win.window().request_redraw();
            }
        }
        run_clock(&self.clock, win.as_weak(), &self.shared);
    }

    /// The shaders' `iSeed`: a number of the tab on screen (each new tab gets the next).
    pub fn set_seed<C: ComponentHandle + 'static>(&self, win: &C, seed: f32) {
        let mut shared = self.shared.borrow_mut();
        if shared.seed != seed {
            shared.seed = seed;
            win.window().request_redraw();
        }
    }

    /// The colors the shaders see (`iBackgroundColor`, `iPalette`…).
    pub fn set_colors(&self, colors: Colors) {
        self.shared.borrow_mut().colors = Some(colors);
    }

    /// The color of the window mascot's mood (`iMoodColor`). Draws again when it changed.
    pub fn set_mood<C: ComponentHandle + 'static>(&self, win: &C, color: [f32; 3]) {
        let mut shared = self.shared.borrow_mut();
        if shared.mood != color {
            shared.mood = color;
            win.window().request_redraw();
        }
    }

    /// The shell rang the bell or a command failed (`iTimeError`).
    pub fn error<C: ComponentHandle + 'static>(&self, win: &C) {
        {
            let mut shared = self.shared.borrow_mut();
            let now = Instant::now();
            shared.error = Some(now);
            shared.kicked = Some(now);
        }
        win.window().request_redraw();
        run_clock(&self.clock, win.as_weak(), &self.shared);
    }
}

/// Starts the clock if the chain is moving; it stops itself once the chain is not.
fn run_clock<C: ComponentHandle + 'static>(clock: &Rc<Timer>, weak: slint::Weak<C>, shared: &Rc<RefCell<Shared>>) {
    if !shared.borrow().moving() {
        clock.stop();
        return;
    }
    if clock.running() {
        return;
    }
    let (me, shared) = (Rc::downgrade(clock), shared.clone());
    clock.start(TimerMode::Repeated, FRAME, move || {
        // drawing: the notifier has the state borrowed, and this frame is asked anyway
        let Ok(state) = shared.try_borrow() else { return };
        if !state.moving() {
            if let Some(me) = me.upgrade() {
                me.stop();
            }
            return;
        }
        if let Some(win) = weak.upgrade()
            && win.window().is_visible()
        {
            win.window().request_redraw();
        }
    });
}

/// The GL objects of the chain, living in the renderer's context.
struct Gl {
    gl: glow::Context,
    /// Needed to draw in a core profile, though the triangle uses no attributes.
    vao: Option<glow::VertexArray>,
    passes: Vec<Pass>,
    /// The area's copy and the output of every pass but the last, alternating.
    textures: [Option<glow::Texture>; 2],
    fbo: Option<glow::Framebuffer>,
    size: (i32, i32),
    start: Instant,
    last: Instant,
    frame: i32,
    /// Cursor now and before its last move (GL pixels: x, top y, width, height), and when
    /// it moved (seconds since `start`).
    cursor: [f32; 4],
    previous: [f32; 4],
    cursor_changed: f32,
    focused: bool,
    focus_changed: f32,
    /// The last error (`iTimeError`), seconds since `start`.
    error: f32,
}

/// One compiled shader and where its inputs go.
struct Pass {
    program: glow::Program,
    at: HashMap<&'static str, glow::UniformLocation>,
    /// What it drew the frame before (`iChannel1`), at `size`; only for a shader that
    /// reads it.
    history: Option<glow::Texture>,
    history_size: (i32, i32),
    /// Where the values of the source's choices go, in the order of `Source::uniforms`.
    choices: Vec<Option<glow::UniformLocation>>,
}

const UNIFORMS: [&str; 24] = [
    "iResolution",
    "iTime",
    "iTimeDelta",
    "iFrame",
    "iMouse",
    "iDate",
    "iChannel0",
    "iChannel1",
    "iTimeError",
    "iMoodColor",
    "iSeed",
    "iChannelResolution",
    "iCurrentCursor",
    "iPreviousCursor",
    "iCurrentCursorColor",
    "iPreviousCursorColor",
    "iTimeCursorChange",
    "iFocus",
    "iTimeFocus",
    "iBackgroundColor",
    "iForegroundColor",
    "iCursorColor",
    "iPalette",
    "uOffset",
];

/// The renderer's GL state that a pass changes, to put back afterwards.
struct Saved {
    program: Option<glow::Program>,
    vao: Option<glow::VertexArray>,
    active_texture: i32,
    texture: Option<glow::Texture>,
    /// What texture unit 1 had (`iChannel1` uses it).
    texture1: Option<glow::Texture>,
    draw_fb: Option<glow::Framebuffer>,
    read_fb: Option<glow::Framebuffer>,
    viewport: [i32; 4],
    scissor: [i32; 4],
    color_mask: [bool; 4],
    caps: [(u32, bool); 4],
}

/// The area in the window's framebuffer: physical pixels, from the bottom (GL's origin),
/// clipped to the window.
#[derive(Debug, PartialEq)]
struct Area {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

/// `logical`: x, y, width and height in logical pixels, from the window's top. `None`
/// when nothing of it is visible.
fn screen_area(logical: (f32, f32, f32, f32), scale: f32, window: (i32, i32)) -> Option<Area> {
    let px = |v: f32| (v * scale).round() as i32;
    let (x, top) = (px(logical.0).max(0), px(logical.1).max(0));
    let right = px(logical.0 + logical.2).min(window.0);
    let bottom = px(logical.1 + logical.3).min(window.1);
    let (w, h) = (right - x, bottom - top);
    (w > 0 && h > 0).then_some(Area { x, y: window.1 - bottom, w, h })
}

/// The cursor as Ghostty passes it: x and the TOP edge's y in pixels from the area's
/// bottom left, width and height.
fn cursor_rect(c: (f32, f32, f32, f32), scale: f32, area_height: i32) -> [f32; 4] {
    [c.0 * scale, area_height as f32 - c.1 * scale, c.2 * scale, c.3 * scale]
}

impl Gl {
    fn new(gl: glow::Context) -> Self {
        // SAFETY: called with the renderer's context current (see `PostFx::attach`).
        let vao = unsafe {
            log::info!(
                "shaders: OpenGL {}, GLSL {}",
                gl.get_parameter_string(glow::VERSION),
                gl.get_parameter_string(glow::SHADING_LANGUAGE_VERSION)
            );
            gl.create_vertex_array().ok()
        };
        let now = Instant::now();
        Gl {
            gl,
            vao,
            passes: Vec::new(),
            textures: [None, None],
            fbo: None,
            size: (0, 0),
            start: now,
            last: now,
            frame: 0,
            cursor: [0.0; 4],
            previous: [0.0; 4],
            cursor_changed: -1000.0,
            focused: true,
            focus_changed: -1000.0,
            error: -1000.0,
        }
    }

    /// Builds the chain anew. One shader that doesn't compile turns them all off (half a
    /// chain is not what was asked for); `errors` says why.
    fn build(&mut self, sources: &[Source]) {
        self.drop_passes();
        for source in sources {
            match self.compile(source) {
                Ok(pass) => {
                    set_error(&source.name, None);
                    self.passes.push(pass);
                }
                Err(e) => {
                    log::warn!("shader {}: {e}", source.name);
                    set_error(&source.name, Some(e));
                    self.drop_passes();
                    return;
                }
            }
        }
        self.frame = 0;
    }

    /// Compiles one shader with the newest GLSL version the driver takes. The error lists
    /// every attempt: which one matters depends on the driver.
    fn compile(&self, source: &Source) -> Result<Pass, String> {
        let mut errors = Vec::new();
        for header in VERSIONS {
            // `#line` keeps the driver's line numbers pointing into the shader's own file
            let fragment = format!("{header}{PRELUDE}#line 1\n{}\n{MAIN}", source.glsl);
            // SAFETY: the renderer's context is current while it notifies.
            match unsafe { self.program(&format!("{header}{VERTEX}"), &fragment) } {
                Ok(program) => {
                    let at = UNIFORMS
                        .iter()
                        // SAFETY: as above
                        .filter_map(|name| unsafe { self.gl.get_uniform_location(program, name) }.map(|l| (*name, l)))
                        .collect();
                    // SAFETY: as above
                    let choices = source.uniforms.iter().map(|(name, _)| unsafe { self.gl.get_uniform_location(program, name) }).collect();
                    return Ok(Pass { program, at, history: None, history_size: (0, 0), choices });
                }
                Err(e) => errors.push(format!("[{}] {e}", header.lines().next().unwrap_or_default())),
            }
        }
        Err(errors.join("\n"))
    }

    unsafe fn program(&self, vertex: &str, fragment: &str) -> Result<glow::Program, String> {
        let gl = &self.gl;
        unsafe {
            let program = gl.create_program()?;
            let mut shaders = Vec::new();
            let mut error = None;
            for (kind, text) in [(glow::VERTEX_SHADER, vertex), (glow::FRAGMENT_SHADER, fragment)] {
                let shader = gl.create_shader(kind)?;
                gl.shader_source(shader, text);
                gl.compile_shader(shader);
                if !gl.get_shader_compile_status(shader) {
                    error.get_or_insert_with(|| gl.get_shader_info_log(shader));
                }
                gl.attach_shader(program, shader);
                shaders.push(shader);
            }
            if error.is_none() {
                gl.link_program(program);
                if !gl.get_program_link_status(program) {
                    error = Some(gl.get_program_info_log(program));
                }
            }
            for shader in shaders {
                gl.detach_shader(program, shader);
                gl.delete_shader(shader);
            }
            match error {
                None => Ok(program),
                Some(e) => {
                    gl.delete_program(program);
                    Err(e.trim().to_string())
                }
            }
        }
    }

    fn drop_passes(&mut self) {
        for pass in self.passes.drain(..) {
            // SAFETY: the program and texture were made in this context, which is current.
            unsafe {
                self.gl.delete_program(pass.program);
                if let Some(t) = pass.history {
                    self.gl.delete_texture(t);
                }
            }
        }
    }

    fn destroy(mut self) {
        self.drop_passes();
        // SAFETY: as above; nothing uses these objects afterwards.
        unsafe {
            for t in self.textures.iter().flatten() {
                self.gl.delete_texture(*t);
            }
            if let Some(f) = self.fbo {
                self.gl.delete_framebuffer(f);
            }
            if let Some(v) = self.vao {
                self.gl.delete_vertex_array(v);
            }
        }
    }

    /// Runs the chain over the area of the frame Slint just drew. `true` when an event
    /// came (the cursor moved, the focus changed): a chain that animates on events goes on.
    fn frame(&mut self, frame: &Frame, scale: f32, window: (i32, i32), shared: &mut Shared) -> bool {
        if shared.changed {
            shared.changed = false;
            self.build(&shared.sources);
        }
        if self.passes.is_empty() {
            return false;
        }
        let Some(area) = screen_area(frame.area, scale, window) else { return false };
        let now = self.start.elapsed().as_secs_f32();
        let mut event = false;
        // a hidden cursor (blinking, a program hiding it) stays where it was: a trail
        // follows moves, not blinks
        if let Some(c) = frame.cursor {
            let cursor = cursor_rect(c, scale, area.h);
            if cursor != self.cursor {
                self.previous = if self.cursor == [0.0; 4] { cursor } else { self.cursor };
                self.cursor = cursor;
                self.cursor_changed = now;
                event = true;
            }
        }
        if frame.focused != self.focused {
            self.focused = frame.focused;
            self.focus_changed = now;
            event = true;
        }
        self.error = shared.error.and_then(|at| at.checked_duration_since(self.start)).map_or(-1000.0, |d| d.as_secs_f32());
        // SAFETY: the renderer's context is current while it notifies; every state changed
        // here is restored before returning.
        unsafe {
            let saved = self.save();
            self.ensure_targets(area.w, area.h);
            self.run(&area, &saved, shared);
            self.restore(&saved);
        }
        event
    }

    unsafe fn save(&self) -> Saved {
        let gl = &self.gl;
        unsafe {
            let active_texture = gl.get_parameter_i32(glow::ACTIVE_TEXTURE);
            gl.active_texture(glow::TEXTURE0);
            let texture = gl.get_parameter_texture(glow::TEXTURE_BINDING_2D);
            gl.active_texture(glow::TEXTURE1);
            let texture1 = gl.get_parameter_texture(glow::TEXTURE_BINDING_2D);
            gl.active_texture(glow::TEXTURE0);
            let mut viewport = [0; 4];
            gl.get_parameter_i32_slice(glow::VIEWPORT, &mut viewport);
            let mut scissor = [0; 4];
            gl.get_parameter_i32_slice(glow::SCISSOR_BOX, &mut scissor);
            let cap = |c| (c, gl.is_enabled(c));
            Saved {
                program: gl.get_parameter_program(glow::CURRENT_PROGRAM),
                vao: gl.get_parameter_vertex_array(glow::VERTEX_ARRAY_BINDING),
                active_texture,
                texture,
                texture1,
                draw_fb: gl.get_parameter_framebuffer(glow::DRAW_FRAMEBUFFER_BINDING),
                read_fb: gl.get_parameter_framebuffer(glow::READ_FRAMEBUFFER_BINDING),
                viewport,
                scissor,
                color_mask: gl.get_parameter_bool_array::<4>(glow::COLOR_WRITEMASK),
                caps: [cap(glow::SCISSOR_TEST), cap(glow::BLEND), cap(glow::STENCIL_TEST), cap(glow::DEPTH_TEST)],
            }
        }
    }

    unsafe fn restore(&self, s: &Saved) {
        let gl = &self.gl;
        unsafe {
            gl.use_program(s.program);
            gl.bind_vertex_array(s.vao);
            gl.active_texture(glow::TEXTURE1);
            gl.bind_texture(glow::TEXTURE_2D, s.texture1);
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, s.texture);
            gl.active_texture(s.active_texture as u32);
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, s.draw_fb);
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, s.read_fb);
            gl.viewport(s.viewport[0], s.viewport[1], s.viewport[2], s.viewport[3]);
            gl.scissor(s.scissor[0], s.scissor[1], s.scissor[2], s.scissor[3]);
            let [r, g, b, a] = s.color_mask;
            gl.color_mask(r, g, b, a);
            for (cap, on) in s.caps {
                if on {
                    gl.enable(cap);
                } else {
                    gl.disable(cap);
                }
            }
        }
    }

    /// The two textures (and the framebuffer that draws into them) at the area's size.
    unsafe fn ensure_targets(&mut self, w: i32, h: i32) {
        let gl = &self.gl;
        unsafe {
            if self.fbo.is_none() {
                self.fbo = gl.create_framebuffer().ok();
            }
            if self.size == (w, h) && self.textures.iter().all(Option::is_some) {
                return;
            }
            for slot in &mut self.textures {
                let texture = match *slot {
                    Some(t) => t,
                    None => match gl.create_texture() {
                        Ok(t) => *slot.insert(t),
                        Err(_) => continue,
                    },
                };
                gl.bind_texture(glow::TEXTURE_2D, Some(texture));
                gl.tex_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    glow::RGBA8 as i32,
                    w,
                    h,
                    0,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(None),
                );
                gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
                gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
                gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
                gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
            }
            self.size = (w, h);
        }
    }

    /// Copies the area out of the frame and draws it back through every pass.
    unsafe fn run(&mut self, area: &Area, saved: &Saved, shared: &Shared) {
        let (colors, mood) = (shared.colors.as_ref(), shared.mood);
        let [Some(first), Some(second)] = self.textures else { return };
        let now = Instant::now();
        let time = now.duration_since(self.start).as_secs_f32();
        let delta = now.duration_since(self.last).as_secs_f32();
        self.last = now;
        let date = date();
        let gl = &self.gl;
        unsafe {
            gl.disable(glow::BLEND);
            gl.disable(glow::STENCIL_TEST);
            gl.disable(glow::DEPTH_TEST);
            gl.bind_vertex_array(self.vao);
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, saved.draw_fb);
            gl.bind_texture(glow::TEXTURE_2D, Some(first));
            gl.copy_tex_sub_image_2d(glow::TEXTURE_2D, 0, 0, 0, area.x, area.y, area.w, area.h);

            let (mut input, mut output) = (first, second);
            let count = self.passes.len();
            let (w, h) = (area.w as f32, area.h as f32);
            for (i, pass) in self.passes.iter_mut().enumerate() {
                let last = i + 1 == count;
                if last {
                    // back into the window, in place; its alpha (the cut corners) stays
                    gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, saved.draw_fb);
                    gl.viewport(area.x, area.y, area.w, area.h);
                    gl.enable(glow::SCISSOR_TEST);
                    gl.scissor(area.x, area.y, area.w, area.h);
                    gl.color_mask(true, true, true, false);
                } else {
                    gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, self.fbo);
                    gl.framebuffer_texture_2d(glow::DRAW_FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(output), 0);
                    gl.viewport(0, 0, area.w, area.h);
                    gl.disable(glow::SCISSOR_TEST);
                    gl.color_mask(true, true, true, true);
                }
                gl.use_program(Some(pass.program));
                gl.bind_texture(glow::TEXTURE_2D, Some(input));
                // what this shader drew the frame before, on unit 1
                let history = if pass.at.contains_key("iChannel1") { history_texture(gl, pass, area.w, area.h) } else { None };
                if let Some(t) = history {
                    gl.active_texture(glow::TEXTURE1);
                    gl.bind_texture(glow::TEXTURE_2D, Some(t));
                    gl.active_texture(glow::TEXTURE0);
                }
                let u = |name: &str| pass.at.get(name);
                gl.uniform_3_f32(u("iResolution"), w, h, 1.0);
                gl.uniform_1_f32(u("iTime"), time);
                gl.uniform_1_f32(u("iTimeDelta"), delta);
                gl.uniform_1_i32(u("iFrame"), self.frame);
                gl.uniform_4_f32(u("iMouse"), 0.0, 0.0, 0.0, 0.0);
                gl.uniform_4_f32_slice(u("iDate"), &date);
                gl.uniform_1_i32(u("iChannel0"), 0);
                gl.uniform_1_i32(u("iChannel1"), 1);
                gl.uniform_1_f32(u("iTimeError"), self.error);
                gl.uniform_3_f32_slice(u("iMoodColor"), &mood);
                gl.uniform_1_f32(u("iSeed"), shared.seed);
                // the source's choices (the chain is built from these sources, in order)
                if let Some(source) = shared.sources.get(i) {
                    for (at, (_, value)) in pass.choices.iter().zip(&source.uniforms) {
                        gl.uniform_1_f32(at.as_ref(), *value);
                    }
                }
                gl.uniform_3_f32_slice(u("iChannelResolution"), &[w, h, 1.0]);
                gl.uniform_4_f32_slice(u("iCurrentCursor"), &self.cursor);
                gl.uniform_4_f32_slice(u("iPreviousCursor"), &self.previous);
                gl.uniform_1_f32(u("iTimeCursorChange"), self.cursor_changed);
                gl.uniform_1_i32(u("iFocus"), i32::from(self.focused));
                gl.uniform_1_f32(u("iTimeFocus"), self.focus_changed);
                if let Some(c) = colors {
                    let cursor = [c.cursor[0], c.cursor[1], c.cursor[2], 1.0];
                    gl.uniform_4_f32_slice(u("iCurrentCursorColor"), &cursor);
                    gl.uniform_4_f32_slice(u("iPreviousCursorColor"), &cursor);
                    gl.uniform_3_f32_slice(u("iBackgroundColor"), &c.background);
                    gl.uniform_3_f32_slice(u("iForegroundColor"), &c.foreground);
                    gl.uniform_3_f32_slice(u("iCursorColor"), &c.cursor);
                    gl.uniform_3_f32_slice(u("iPalette"), c.palette.as_flattened());
                }
                let offset = if last { (area.x as f32, area.y as f32) } else { (0.0, 0.0) };
                gl.uniform_2_f32(u("uOffset"), offset.0, offset.1);
                gl.draw_arrays(glow::TRIANGLES, 0, 3);
                if let Some(t) = history {
                    // keep what was just drawn for the next frame
                    let (fb, x, y) = if last { (saved.draw_fb, area.x, area.y) } else { (self.fbo, 0, 0) };
                    gl.bind_framebuffer(glow::READ_FRAMEBUFFER, fb);
                    gl.active_texture(glow::TEXTURE1);
                    gl.bind_texture(glow::TEXTURE_2D, Some(t));
                    gl.copy_tex_sub_image_2d(glow::TEXTURE_2D, 0, 0, 0, x, y, area.w, area.h);
                    gl.active_texture(glow::TEXTURE0);
                }
                std::mem::swap(&mut input, &mut output);
            }
        }
        self.frame = self.frame.wrapping_add(1);
    }
}

/// The pass's `iChannel1` texture at `w`×`h`, made (black) when missing or resized.
///
/// # Safety
/// The context `gl` belongs to must be current.
unsafe fn history_texture(gl: &glow::Context, pass: &mut Pass, w: i32, h: i32) -> Option<glow::Texture> {
    unsafe {
        if pass.history.is_some() && pass.history_size == (w, h) {
            return pass.history;
        }
        let texture = match pass.history {
            Some(t) => t,
            None => *pass.history.insert(gl.create_texture().ok()?),
        };
        gl.active_texture(glow::TEXTURE1);
        gl.bind_texture(glow::TEXTURE_2D, Some(texture));
        let black = vec![0u8; (w * h * 4).max(0) as usize];
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA8 as i32,
            w,
            h,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(Some(&black)),
        );
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
        gl.active_texture(glow::TEXTURE0);
        pass.history_size = (w, h);
        Some(texture)
    }
}

/// Shadertoy's `iDate`: year, month (from 0), day and seconds since midnight, local time.
fn date() -> [f32; 4] {
    use chrono::{Datelike, Timelike};
    let now = chrono::Local::now();
    let seconds = now.num_seconds_from_midnight() as f32 + now.nanosecond() as f32 / 1e9;
    [now.year() as f32, now.month0() as f32, now.day() as f32, seconds]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_area_is_flipped_scaled_and_clipped() {
        // below a 40 px title bar, in a 100×80 window at 2×: GL counts from the bottom
        let a = screen_area((0.0, 40.0, 100.0, 40.0), 2.0, (200, 160)).unwrap();
        assert_eq!(a, Area { x: 0, y: 0, w: 200, h: 80 });
        // past the window's edge: cut to what is inside
        let a = screen_area((10.0, 10.0, 500.0, 500.0), 1.0, (200, 160)).unwrap();
        assert_eq!(a, Area { x: 10, y: 0, w: 190, h: 150 });
        assert!(screen_area((0.0, 0.0, 0.0, 10.0), 1.0, (200, 160)).is_none());
    }

    #[test]
    fn cursor_is_given_by_its_top_edge_from_the_bottom() {
        // a 10×20 cell at (30, 40) of a 200 px tall area, at 2×
        assert_eq!(cursor_rect((30.0, 40.0, 10.0, 20.0), 2.0, 200), [60.0, 120.0, 20.0, 40.0]);
    }

    #[test]
    fn palette_follows_xterm_after_the_16_colors() {
        let c = Colors::new([0.0; 3], [1.0; 3], [1.0; 3], [[0.5; 3]; 16]);
        assert_eq!(c.palette[15], [0.5; 3]);
        // 16 is the cube's black, 231 its white, 232 the first gray
        assert_eq!(c.palette[16], [0.0; 3]);
        assert_eq!(c.palette[231], [1.0; 3]);
        assert_eq!(c.palette[232], [8.0 / 255.0; 3]);
    }

    #[test]
    fn only_shaders_reading_the_clock_redraw() {
        let src = |glsl: &str| Source { name: String::new(), glsl: glsl.into(), uniforms: Vec::new() };
        // the built-in ones that only follow events say so; only the ambient ones (that move
        // on their own) ask for every frame
        let ambient = ["rain", "scan_sweep", "mood_vignette", "pixel_scenes"];
        for s in super::super::shaders::builtin() {
            let redraw = src(&s.glsl).redraw();
            assert_eq!(redraw == Redraw::Always, ambient.contains(&s.key.as_str()), "{}: {redraw:?}", s.key);
        }
        assert_eq!(src("void mainImage(out vec4 c, in vec2 p) { c = vec4(sin(iTime)); }").redraw(), Redraw::Always);
        // named only in comments: still static; a division is not a comment
        assert_eq!(src("// iTime\n/* iFrame */ void mainImage(out vec4 c, in vec2 p) { c = vec4(p / 2.0, 0, 1); }").redraw(), Redraw::Still);
        assert_eq!(src("float t = 1.0 / 2.0 * iTime; // clock").redraw(), Redraw::Always);
        // a cursor shader without the pragma (Ghostty's): every frame, as Ghostty does
        assert_eq!(src("float age = iTime - iTimeCursorChange;").redraw(), Redraw::Always);
        assert_eq!(src("#pragma miniutils events\nfloat age = iTime - iTimeCursorChange;").redraw(), Redraw::Events);
    }
}
