extern crate byte_slice_cast;
#[macro_use]
extern crate clap;
#[macro_use]
extern crate failure;
extern crate gleam;
#[macro_use]
extern crate gstreamer as gst;
extern crate gstreamer_app as gst_app;
extern crate gstreamer_audio as gst_audio;
extern crate gstreamer_video as gst_video;
#[macro_use]
extern crate log;
extern crate chrono;
extern crate env_logger;
extern crate image;
extern crate notify;
extern crate sdl2;
#[macro_use]
extern crate serde_derive;
extern crate glsl_include;
extern crate lazy_static;
extern crate toml;
extern crate walkdir;

mod audio;
mod config;
mod effect;
mod effect_player;
mod error;
mod file_stream;
mod gl;
mod keyboard;
mod mouse;
mod platform;
mod resource;
mod stream;
mod video;

use crate::effect_player::EffectPlayer;
use crate::error::Error;
use crate::file_stream::FileStream;
use crate::platform::Platform;
use clap::{App, Arg};
use glsl_include::Context as GlslIncludeContex;
use sdl2::video::GLProfile;
use std::collections::BTreeMap;
use sdl2::event::Event;
use sdl2::keyboard::Keycode;
use std::env;
use std::process;
use std::result;
use std::time::{Duration, Instant};
use walkdir::{DirEntry, WalkDir};

/// Our type alias for handling errors throughout grimoire
type Result<T> = result::Result<T, failure::Error>;

fn main() {
    if let Err(err) = try_main() {
        // Print the error, including all of its underlying causes.
        error!("{}", pretty_error(&err));

        // If we get a non-empty backtrace (e.g., RUST_BACKTRACE=1 is set),
        // then show it.
        let backtrace = err.backtrace().to_string();
        if !backtrace.trim().is_empty() {
            eprintln!("{}", backtrace);
        }
        process::exit(1);
    }
}

fn try_main() -> Result<()> {
    env_logger::init();
    {
        let args: Vec<String> = env::args().collect();
        info!("{:?}", args);
    }
    let matches = App::new("grimoire")
        .version(crate_version!())
        .author(crate_authors!())
        .about("https://github.com/jshrake/grimoire")
        .arg(
            Arg::with_name("config")
                .help("path to the toml configuration file, or directory containing grim.toml")
                .required(false)
                .index(1),
        )
        .arg(
            Arg::with_name("width")
                .help("window pixel width")
                .takes_value(true)
                .default_value("768")
                .long("width")
                .requires("height"),
        )
        .arg(
            Arg::with_name("height")
                .help("window pixel height")
                .takes_value(true)
                .default_value("432")
                .long("height")
                .requires("width"),
        )
        .arg(
            Arg::with_name("gl")
                .help("opengl version")
                .takes_value(true)
                .possible_values(&[
                    "330", "400", "410", "420", "430", "440", "450", "460", "es2", "es3",
                ])
                .default_value("410")
                .long("gl"),
        )
        .arg(
            Arg::with_name("fps")
                .help("target fps")
                .takes_value(true)
                .default_value("0")
                .long("fps"),
        )
        .get_matches();
    let width_str = matches.value_of("width").unwrap();
    let height_str = matches.value_of("height").unwrap();
    let config_path_str = matches.value_of("config").unwrap_or("./grim.toml");
    let target_fps_str = matches.value_of("fps").unwrap();
    let gl_str = matches.value_of("gl").unwrap();
    let width = width_str
        .parse::<u32>()
        .expect("Expected width command-line argument to be u32");
    let height = height_str
        .parse::<u32>()
        .expect("Expected height command-line argument to be u32");
    let target_fps = target_fps_str
        .parse::<u32>()
        .expect("Expected fps command-line argument to be u32");
    let (gl_major, gl_minor, gl_profile, glsl_version) = match gl_str {
        "330" => (3, 3, GLProfile::Core, "#version 330"),
        "400" => (4, 0, GLProfile::Core, "#version 400"),
        "410" => (4, 1, GLProfile::Core, "#version 410"),
        "420" => (4, 2, GLProfile::Core, "#version 420"),
        "430" => (4, 3, GLProfile::Core, "#version 430"),
        "440" => (4, 4, GLProfile::Core, "#version 440"),
        "450" => (4, 5, GLProfile::Core, "#version 450"),
        "460" => (4, 6, GLProfile::Core, "#version 460"),
        "es2" => (2, 0, GLProfile::GLES, "#version 100"),
        "es3" => (3, 0, GLProfile::GLES, "#version 300"),
        _ => unreachable!(),
    };

    // Call gst::init BEFORE changing the cwd
    // On windows 10, this reduces gst::init from ~7 seconds to ~50 ms
    // TODO(jshrake): Why? Is there an issue with how we see the cwd on windows?
    let gst_init_duration = Instant::now();
    gst::init()?;
    let gst_init_duration = gst_init_duration.elapsed();
    info!("gst::init took {:?}", gst_init_duration);

    // Resolve the config path early and exit if not found
    let mut absolute_config_path = std::path::Path::new(config_path_str)
        .canonicalize()
        .map_err(|err| {
            format_err!(
                "[PLATFORM] Error loading config file {:?}: {}",
                config_path_str,
                err
            )
        })?;
    if absolute_config_path.is_dir() {
        absolute_config_path.push("grim.toml");
    }
    let desired_cwd = absolute_config_path
        .parent()
        .expect("Expected config file to have parent directory");
    env::set_current_dir(&desired_cwd).expect("env::set_current_dir failed");
    info!("Current working directory: {:?}", desired_cwd);

    let sdl_context = sdl2::init().map_err(Error::sdl2)?;
    let _joystick_subsystem = sdl_context.joystick().map_err(Error::sdl2)?;
    let video_subsystem = sdl_context.video().map_err(Error::sdl2)?;
    let gl_attr = video_subsystem.gl_attr();
    gl_attr.set_context_version(gl_major, gl_minor);
    gl_attr.set_context_profile(gl_profile);
    // TODO(jshrake): These should be config/cli driven
    gl_attr.set_depth_size(24);
    gl_attr.set_framebuffer_srgb_compatible(true);
    gl_attr.set_multisample_buffers(1);
    gl_attr.set_multisample_samples(4);

    let window = video_subsystem
        .window("grimoire", width, height)
        .opengl()
        .resizable()
        .build()?;

    let _ctx = window.gl_create_context().map_err(Error::sdl2)?;
    debug_assert_eq!(gl_attr.context_profile(), gl_profile);
    debug_assert_eq!(gl_attr.context_version(), (gl_major, gl_minor));
    let gl = unsafe {
        gl::GlesFns::load_with(|addr| video_subsystem.gl_get_proc_address(addr) as *const _)
    };

    match video_subsystem.gl_set_swap_interval(sdl2::video::SwapInterval::LateSwapTearing) {
        Ok(_) => {
            info!("vsync late swap tearing enabled");
        }
        Err(_) => match video_subsystem.gl_set_swap_interval(sdl2::video::SwapInterval::VSync) {
            Ok(_) => {
                info!("vsync enabled");
            }
            Err(_) => {
                info!("vsync disabled");
            }
        },
    }

    let mut event_pump = sdl_context.event_pump().map_err(Error::sdl2)?;

    // Log Welcome Message + GL information
    info!(
        "Requested GL profile: {:?}, got {:?}",
        gl_profile,
        gl_attr.context_profile()
    );
    info!(
        "Requested GL version: {:?}, got {:?}",
        (gl_major, gl_minor),
        gl_attr.context_version()
    );
    {
        let vendor = gl.get_string(gl::VENDOR);
        let renderer = gl.get_string(gl::RENDERER);
        let version = gl.get_string(gl::VERSION);
        let shading_lang_version = gl.get_string(gl::SHADING_LANGUAGE_VERSION);
        let extension_count = unsafe {
            let mut extension_count: [i32; 1] = [0];
            gl.get_integer_v(gl::NUM_EXTENSIONS, &mut extension_count);
            extension_count[0]
        };
        let extensions: Vec<String> = (0..extension_count)
            .map(|i| gl.get_string_i(gl::EXTENSIONS, i as u32))
            .collect();
        info!("GL VENDOR:    {}", vendor);
        info!("GL RENDERER:  {}", renderer);
        info!("GL VERSION:   {}", version);
        info!("GLSL VERSION: {}", shading_lang_version);
        debug!("EXTENSIONS: {:?}", extensions);
    }
    let mut platform = Platform {
        events: &mut event_pump,
        gl: gl.clone(),
        window_resolution: window.size(),
        time_delta: Duration::from_secs(0),
        keyboard: [0; 256],
    };

    fn is_glsl(entry: &DirEntry) -> bool {
        entry
            .path()
            .extension()
            .map(|s| s == "glsl" || s == "vert" || s == "frag" || s == "vs" || s == "fs")
            .unwrap_or(false)
    }

    let mut shader_include_streams = BTreeMap::new();
    for entry in WalkDir::new(".").into_iter().filter_map(|e| e.ok()) {
        if entry.path().is_file() && is_glsl(&entry) {
            let path = std::fs::canonicalize(&entry.path())?;
            let glsl_include_path = String::from(entry.file_name().to_str().unwrap());
            shader_include_streams.insert(glsl_include_path, FileStream::new(path.as_path())?);
        }
    }
    let glsl_include_ctx = GlslIncludeContex::new();
    let mut player = EffectPlayer::new(
        absolute_config_path.as_path(),
        glsl_version.to_string(),
        shader_include_streams,
        glsl_include_ctx,
    )?;
    player.play()?;
    let mut frame_count = 0;
    let mut total_elapsed: Duration = Default::default();
    let frame_window = 30;

    // SDL events
    'running: loop {
        platform.keyboard = [0; 256];
        let scancodes : Vec<_> = platform.events.keyboard_state().pressed_scancodes().map(|sc| sc).collect();
        for scancode in scancodes {
            let keycode = sdl2::keyboard::Keycode::from_scancode(scancode);
            if let Some(kc) = keycode {
                let text = kc.name();
                let c = match text.as_ref() {
                    "Space" => ' ',
                    "Left" => 37 as char,
                    "Up" => 38 as char,
                    "Right" => 39 as char,
                    "Down" => 40 as char,
                    "Return" => 13 as char,
                    "Backspace" => 8 as char,
                    _ => text.chars().next().unwrap(),
                };
                if c < ' ' || c > '~' {
                    continue;
                }
                let idx = c.to_ascii_uppercase() as usize;
                //info!("{}: {} {}", frame_count, text, idx);
                platform.keyboard[idx] = 255;
            }
        }
        for event in platform.events.poll_iter() {
            match event {
                Event::Window { win_event, .. } => match win_event {
                    _ => {}
                },
                Event::Quit { .. }
                | Event::KeyDown {
                    keycode: Some(Keycode::Escape),
                    ..
                } => {
                    break 'running;
                }
                Event::KeyDown {
                    keycode: Some(Keycode::F1),
                    ..
                } => player.toggle_play()?,
                Event::KeyDown {
                    keycode: Some(Keycode::F2),
                    ..
                } => {
                    player.pause()?;
                    player.step_backward(platform.time_delta);
                }
                Event::KeyDown {
                    keycode: Some(Keycode::F3),
                    ..
                } => {
                    player.pause()?;
                    player.step_forward(platform.time_delta);
                }
                Event::KeyDown {
                    keycode: Some(Keycode::F4),
                    ..
                } => {
                    player.restart()?;
                }
                _ => {}
            }
        }
        let now = Instant::now();
        match player.tick(&mut platform) {
            Err(err) => error!("{}", pretty_error(&failure::Error::from(err))),
            _ => {}
        }
        let elapsed_duration = now.elapsed();
        let elapsed = duration_to_float_secs(elapsed_duration);
        if target_fps > 0 {
            let fps = target_fps as f32;
            let mpf = 1.0 / fps;
            let cushion = mpf * 0.05;
            let elapsed = elapsed + cushion;
            let sleep = if elapsed > mpf { 0.0 } else { mpf - elapsed };
            let sleep_duration = Duration::from_micros((1_000_000.0 * sleep) as u64);
            std::thread::sleep(sleep_duration);
            debug!("thread::sleep({:?}), target FPS = {}", sleep_duration, fps);
        }
        window.gl_swap_window();
        platform.time_delta = now.elapsed();
        platform.window_resolution = window.size();
        frame_count += 1;
        total_elapsed += platform.time_delta;
        if frame_count > frame_window {
            debug!(
                "[PLATFORM] Average frame time over last {} frames: {} seconds",
                frame_window,
                duration_to_float_secs(total_elapsed) / frame_window as f32
            );
            frame_count = Default::default();
            total_elapsed = Default::default();
        }
    }
    Ok(())
}
fn duration_to_float_secs(duration: Duration) -> f32 {
    duration.as_secs() as f32 + duration.subsec_nanos() as f32 * 1e-9
}

/// Return a prettily formatted error, including its entire causal chain.
fn pretty_error(err: &failure::Error) -> String {
    let mut pretty = String::new();
    pretty.push_str(&err.to_string());
    let mut prev = err.as_fail();
    while let Some(next) = prev.cause() {
        pretty.push_str("\n");
        pretty.push_str(&next.to_string());
        prev = next;
    }
    pretty
}
