#![forbid(unsafe_code)]

use std::io::Read;
use std::io::Write;
use std::{env, process};

#[cfg(target_os = "linux")]
use arboard::SetExtLinux;
use arboard::{Clipboard, ImageData};
use error_iter::ErrorIter as _;
use line::draw_line;
use log::error;
use pixels::{Error, Pixels, SurfaceTexture};
use rectangle::draw_rect_borders;
use rectangle::draw_rect_filled;
use screenshots::Screen;
use serde::{Deserialize, Serialize};
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, Event, KeyboardInput, VirtualKeyCode, WindowEvent};
use winit::event_loop::{ControlFlow, EventLoop};
use winit::window::Fullscreen;
use winit::window::WindowBuilder;
use winit_input_helper::WinitInputHelper;

const BORDER_COLOR: (u8, u8, u8, u8) = (255, 0, 255, 255);

mod blend;
mod circle;
mod line;
mod rectangle;

const DAEMONIZE_ARG: &str = "__internal_daemonize";

#[derive(Serialize, Deserialize, Debug)]
struct Image {
    pub width: usize,
    pub height: usize,
    pub bytes: Vec<u8>,
}

fn main() -> Result<(), Error> {
    if env::args().nth(1).as_deref() == Some("--help") {
        println!(
            r#"
Usage: 
  Currently it can be run only through "birdy" executable(from terminal, app launcher(e.g. rofi), bound to a hotkey):

  # bash
  birdy

  # e.g. sway
  bindsym $mod+Shift+p exec birdy


Hotkeys:
  Enter - take a screenshot of selected area, save to a clipboard and exit
  f - take a screenshot where selected area is focused, save to a clipboard and exit

  l - draw a line
  r - draw a rectangular border
  p - draw a filled rectangle
  t - toggle latest drawn shape between filled/not filled states

  Esc - exit
"#
        );

        return Ok(());
    }

    #[cfg(target_os = "linux")]
    if env::args().nth(1).as_deref() == Some(DAEMONIZE_ARG) {
        let mut buf = String::new();
        std::io::stdin()
            .lock()
            .read_to_string(&mut buf)
            .expect("passed image read");
        let passed_img: Option<Image> = serde_json::from_str(&buf).ok();
        if let Some(saved_image) = passed_img {
            let img = ImageData {
                width: saved_image.width,
                height: saved_image.height,
                bytes: saved_image.bytes.into(),
            };
            Clipboard::new()
                .unwrap()
                .set()
                .wait()
                .image(img)
                .expect("passed image copied");
        }
        return Ok(());
    }

    env_logger::init();
    let event_loop = EventLoop::new();
    let mut input = WinitInputHelper::new();
    let window = {
        WindowBuilder::new()
            .with_title("Hello Pixels")
            .with_fullscreen(Some(Fullscreen::Borderless(None)))
            .with_maximized(true)
            .build(&event_loop)
            .unwrap()
    };

    let mut pixels = {
        let window_size = window.inner_size();
        let surface_texture = SurfaceTexture::new(window_size.width, window_size.height, &window);
        Pixels::new(window_size.width, window_size.height, surface_texture)?
    };

    let mut screenshot = Screenshot::new(
        window.inner_size().width as usize,
        window.inner_size().height as usize,
    );

    event_loop.run(move |event, _, control_flow| {
        if let Event::RedrawRequested(_) = event {
            screenshot.draw(pixels.frame_mut());

            if let Err(err) = pixels.render() {
                log_error("pixels.render", err);
                *control_flow = ControlFlow::Exit;
                return;
            }
        }

        match event {
            Event::WindowEvent {
                event: WindowEvent::MouseInput { state, .. },
                ..
            } => {
                if let ElementState::Pressed = state {
                    screenshot.on_mouse_pressed();
                } else {
                    screenshot.on_mouse_released();
                }

                window.request_redraw();
            }

            Event::WindowEvent {
                event: WindowEvent::CursorMoved { position, .. },
                ..
            } => {
                screenshot.on_mouse_move(position);

                if screenshot.is_resizing {
                    window.request_redraw();
                }
            }

            Event::WindowEvent {
                event:
                    WindowEvent::KeyboardInput {
                        input:
                            KeyboardInput {
                                state: ElementState::Pressed,
                                virtual_keycode,
                                ..
                            },
                        ..
                    },
                ..
            } => {
                if let Some(VirtualKeyCode::Return) = virtual_keycode {
                    screenshot.save_image_to_clipboard(screenshot.get_clipped_image());
                    *control_flow = ControlFlow::Exit;
                    return;
                }
                if let Some(VirtualKeyCode::F) = virtual_keycode {
                    screenshot.save_image_to_clipboard(screenshot.get_focused_image());
                    *control_flow = ControlFlow::Exit;
                    return;
                }

                if let Some(VirtualKeyCode::L) = virtual_keycode {
                    screenshot.draw_mode = Some(DrawMode::Line);
                }
                if let Some(VirtualKeyCode::R) = virtual_keycode {
                    screenshot.draw_mode = Some(DrawMode::RectBorder);
                }
                if let Some(VirtualKeyCode::P) = virtual_keycode {
                    screenshot.draw_mode = Some(DrawMode::RectFilled);
                }
                if let Some(VirtualKeyCode::T) = virtual_keycode {
                    screenshot.toggle_filling_latest();
                }

                window.request_redraw();
            }

            _ => {}
        }

        // Handle input events
        if input.update(&event) {
            if input.key_pressed(VirtualKeyCode::Escape) || input.close_requested() {
                *control_flow = ControlFlow::Exit;
                return;
            }

            // Resize the window
            if let Some(size) = input.window_resized() {
                if let Err(err) = pixels.resize_surface(size.width, size.height) {
                    log_error("pixels.resize_surface", err);
                    *control_flow = ControlFlow::Exit;
                    return;
                }
                if let Err(err) = pixels.resize_buffer(size.width, size.height) {
                    log_error("pixels.resize_buffer", err);
                    *control_flow = ControlFlow::Exit;
                    return;
                };
                screenshot.resize_viewport(size.width as usize, size.height as usize);
            }

            window.request_redraw();
        }
    });
}

fn log_error<E: std::error::Error + 'static>(method_name: &str, err: E) {
    error!("{method_name}() failed: {err}");
    for source in err.sources().skip(1) {
        error!("  Caused by: {source}");
    }
}

struct Screenshot {
    original_screenshot: Vec<u8>,
    modified_screenshot: Vec<u8>,
    p0: (usize, usize),
    p1: (usize, usize),
    width: usize,
    height: usize,

    is_resizing: bool,
    top_border_resized: bool,
    right_border_resized: bool,
    bottom_border_resized: bool,
    left_border_resized: bool,

    draw_mode: Option<DrawMode>,
    drawing_item: Option<DrawnItem>,
    drawn_items: Vec<DrawnItem>,

    mouse_coordinates: Option<PhysicalPosition<f64>>,
}

impl Screenshot {
    fn new(width: usize, height: usize) -> Self {
        let screens = Screen::all().unwrap();
        let original_screenshot = if let Some(screen) = screens.get(0) {
            let image = screen.capture().unwrap();
            image.to_vec()
        } else {
            panic!("can't find an available screen for a screenshot");
        };

        Self {
            original_screenshot: original_screenshot.clone(),
            modified_screenshot: original_screenshot,

            is_resizing: false,
            top_border_resized: false,
            right_border_resized: false,
            bottom_border_resized: false,
            left_border_resized: false,

            draw_mode: None,
            drawing_item: None,
            drawn_items: vec![],

            p0: (0, 0),
            p1: (width, height),
            width,
            height,
            mouse_coordinates: None,
        }
    }

    pub fn resize_viewport(&mut self, width: usize, height: usize) {
        *self = Self::new(width, height);
    }

    fn get_focused_image(&self) -> Image {
        Image {
            width: self.width,
            height: self.height,
            bytes: self.modified_screenshot.clone(),
        }
    }

    fn get_clipped_image(&self) -> Image {
        let mut clipped_image = vec![];
        for y in self.p0.1 + 1..self.p1.1 - 1 {
            for x in self.p0.0 + 1..self.p1.0 - 1 {
                clipped_image.push(self.modified_screenshot[y * (self.width * 4) + (x * 4)]);
                clipped_image.push(self.modified_screenshot[y * (self.width * 4) + (x * 4) + 1]);
                clipped_image.push(self.modified_screenshot[y * (self.width * 4) + (x * 4) + 2]);
                clipped_image.push(self.modified_screenshot[y * (self.width * 4) + (x * 4) + 3]);
            }
        }

        Image {
            width: self.p1.0 - self.p0.0 - 2,
            height: self.p1.1 - self.p0.1 - 2,
            bytes: clipped_image,
        }
    }

    pub fn save_image_to_clipboard(&self, image: Image) {
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        {
            let mut ctx = Clipboard::new().unwrap();

            let img_data = ImageData {
                width: image.width,
                height: image.height,
                bytes: image.bytes.clone().into(),
            };
            ctx.set_image(img_data).unwrap();
        }

        #[cfg(target_os = "linux")]
        {
            let mut child = process::Command::new(env::current_exe().unwrap())
                .arg(DAEMONIZE_ARG)
                .stdin(process::Stdio::piped())
                .stdout(process::Stdio::null())
                .stderr(process::Stdio::null())
                .current_dir("/")
                .spawn()
                .unwrap();

            let mut stdin = child.stdin.take().expect("Failed to open stdin");
            stdin
                .write_all(serde_json::to_string(&image).unwrap().as_bytes())
                .expect("Failed to write to stdin");
        }
    }

    fn draw(&mut self, pixels: &mut [u8]) {
        self.modified_screenshot = self.original_screenshot.clone();
        self.draw_boundaries();
        self.darken_not_selected_area();

        for draw_item in self.drawn_items.clone() {
            self.draw_draw_item(&draw_item);
        }

        if let Some(drawing_item) = self.drawing_item {
            self.draw_draw_item(&drawing_item);
        }

        if pixels.len() == self.modified_screenshot.len() {
            pixels.copy_from_slice(&self.modified_screenshot);
        }
    }

    fn draw_draw_item(&mut self, draw_item: &DrawnItem) {
        match draw_item {
            DrawnItem::Line((x0, y0), (x1, y1)) => {
                draw_line(
                    &mut self.modified_screenshot,
                    *x0,
                    *y0,
                    *x1,
                    *y1,
                    self.width,
                    BORDER_COLOR,
                );
            }
            DrawnItem::RectBorder((x0, y0), (x1, y1)) => {
                draw_rect_borders(
                    &mut self.modified_screenshot,
                    *x0,
                    *y0,
                    *x1,
                    *y1,
                    self.width,
                    BORDER_COLOR,
                );
            }
            DrawnItem::RectFilled((x0, y0), (x1, y1)) => {
                draw_rect_filled(
                    &mut self.modified_screenshot,
                    *x0,
                    *y0,
                    *x1,
                    *y1,
                    self.width,
                    BORDER_COLOR,
                );
            }
        }
    }

    fn draw_boundaries(&mut self) {
        draw_rect_borders(
            &mut self.modified_screenshot,
            self.p0.0,
            self.p0.1,
            self.p1.0,
            self.p1.1,
            self.width,
            BORDER_COLOR,
        );
    }

    fn darken_not_selected_area(&mut self) {
        for y in 0..self.height {
            for x in 0..self.width {
                if x < self.p0.0 || x > self.p1.0 || y < self.p0.1 || y > self.p1.1 {
                    self.modified_screenshot[y * (self.width * 4) + (x * 4) + 3] = 100;
                }
            }
        }
    }

    pub fn toggle_filling_latest(&mut self) {
        if let Some(item) = self.drawn_items.pop() {
            let filled_item = self.toggle_item_filling(&item);
            self.drawn_items.push(filled_item);
        }
    }

    pub fn toggle_item_filling(&mut self, draw_item: &DrawnItem) -> DrawnItem {
        match draw_item {
            DrawnItem::Line(..) => *draw_item,
            DrawnItem::RectBorder(p0, p1) => DrawnItem::RectFilled(*p0, *p1),
            DrawnItem::RectFilled(p0, p1) => DrawnItem::RectBorder(*p0, *p1),
        }
    }

    pub fn on_mouse_move(&mut self, coordinates: PhysicalPosition<f64>) {
        self.mouse_coordinates = Some(coordinates);

        if self.is_resizing && self.top_border_resized {
            self.p0.1 = self.mouse_coordinates.unwrap().y as usize;
        } else if self.is_resizing && self.right_border_resized {
            self.p1.0 = self.mouse_coordinates.unwrap().x as usize;
        } else if self.is_resizing && self.bottom_border_resized {
            self.p1.1 = self.mouse_coordinates.unwrap().y as usize;
        } else if self.is_resizing && self.left_border_resized {
            self.p0.0 = self.mouse_coordinates.unwrap().x as usize;
        } else {
            match self.draw_mode {
                Some(DrawMode::Line) => {
                    if let (Some(DrawnItem::Line(_, p1)), Some(PhysicalPosition { x, y })) =
                        (&mut self.drawing_item, self.mouse_coordinates)
                    {
                        *p1 = (x as usize, y as usize);
                    }
                }
                Some(DrawMode::RectBorder) => {
                    if let (Some(DrawnItem::RectBorder(_, p1)), Some(PhysicalPosition { x, y })) =
                        (&mut self.drawing_item, self.mouse_coordinates)
                    {
                        *p1 = (x as usize, y as usize);
                    }
                }
                Some(DrawMode::RectFilled) => {
                    if let (Some(DrawnItem::RectFilled(_, p1)), Some(PhysicalPosition { x, y })) =
                        (&mut self.drawing_item, self.mouse_coordinates)
                    {
                        *p1 = (x as usize, y as usize);
                    }
                }
                None => {}
            }
        }
    }

    pub fn on_mouse_pressed(&mut self) {
        if let Some(PhysicalPosition { x, y }) = self.mouse_coordinates {
            let x = x as usize;
            let y = y as usize;

            // top resize
            if x > self.p0.0
                && x < self.p1.0
                && y >= self.p0.1.saturating_sub(10)
                && y <= self.p0.1 + 10
            {
                self.is_resizing = true;
                self.top_border_resized = true;
            // right resize
            } else if y > self.p0.1
                && y < self.p1.1
                && x >= self.p1.0.saturating_sub(10)
                && x <= self.p1.0 + 10
            {
                self.is_resizing = true;
                self.right_border_resized = true;
            }
            // bottom resize
            else if x > self.p0.0
                && x < self.p1.0
                && y >= self.p1.1.saturating_sub(10)
                && y <= self.p1.1 + 10
            {
                self.is_resizing = true;
                self.bottom_border_resized = true;
            }
            // left resize
            else if y > self.p0.1
                && y < self.p1.1
                && x >= self.p0.0.saturating_sub(10)
                && x <= self.p0.0 + 10
            {
                self.is_resizing = true;
                self.left_border_resized = true;
            } else {
                match self.draw_mode {
                    Some(DrawMode::Line) => {
                        self.drawing_item = Some(DrawnItem::Line((x, y), (x, y)));
                    }
                    Some(DrawMode::RectBorder) => {
                        self.drawing_item = Some(DrawnItem::RectBorder((x, y), (x, y)));
                    }
                    Some(DrawMode::RectFilled) => {
                        self.drawing_item = Some(DrawnItem::RectFilled((x, y), (x, y)));
                    }
                    None => {}
                }
            }
        }
    }

    pub fn on_mouse_released(&mut self) {
        self.is_resizing = false;
        self.top_border_resized = false;
        self.right_border_resized = false;
        self.bottom_border_resized = false;
        self.left_border_resized = false;

        match self.draw_mode {
            Some(DrawMode::Line) => {
                if let (Some(DrawnItem::Line(p0, _)), Some(PhysicalPosition { x, y })) =
                    (self.drawing_item, self.mouse_coordinates)
                {
                    self.drawn_items
                        .push(DrawnItem::Line(p0, (x as usize, y as usize)));
                    self.drawing_item = None;
                }
            }
            Some(DrawMode::RectBorder) => {
                if let (Some(DrawnItem::RectBorder(p0, _)), Some(PhysicalPosition { x, y })) =
                    (self.drawing_item, self.mouse_coordinates)
                {
                    self.drawn_items
                        .push(DrawnItem::RectBorder(p0, (x as usize, y as usize)));
                    self.drawing_item = None;
                }
            }
            Some(DrawMode::RectFilled) => {
                if let (Some(DrawnItem::RectFilled(p0, _)), Some(PhysicalPosition { x, y })) =
                    (self.drawing_item, self.mouse_coordinates)
                {
                    self.drawn_items
                        .push(DrawnItem::RectFilled(p0, (x as usize, y as usize)));
                    self.drawing_item = None;
                }
            }
            None => {}
        }

        self.draw_mode = None;
    }
}

enum DrawMode {
    Line,
    RectBorder,
    RectFilled,
}

#[derive(Clone, Copy)]
enum DrawnItem {
    Line((usize, usize), (usize, usize)),
    RectBorder((usize, usize), (usize, usize)),
    RectFilled((usize, usize), (usize, usize)),
}
