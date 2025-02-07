use std::rc::Rc;
use std::cell::RefCell;

use gtk4::prelude::*;
use gtk4::{
    glib,
    Application,
    ApplicationWindow,
    Label,
    DrawingArea,
    GestureDrag,
    FileFilter,
    FileDialog,
    EventControllerScroll,
    EventControllerScrollFlags,
};
use gtk4::gio::{ListStore, Menu, SimpleAction, Cancellable};
use gtk4::cairo::{ImageSurface, ImageSurfaceData, Format, Filter, Matrix};
use gtk4::gdk::BUTTON_SECONDARY;
use glib::{Type, Propagation};

use quadio_core as core;

mod waveform;

struct ImageWrapper {
    pub image: ImageSurface,
}

impl waveform::MutSlice for ImageWrapper {
    type Output<'a> = ImageSurfaceData<'a>;

    fn mut_slice<'a>(&'a mut self) -> Self::Output<'a> {
        self.image.data().unwrap()
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ViewTransform {
    offset: f64,
    zoom_scale: f64,
    zoom_pow: i32,
    zoom_pow_min: i32,
    zoom_pow_max: i32,
}

impl ViewTransform {
    pub fn new(zoom_pow_min: i32, zoom_pow_max: i32, zoom_scale: f64) -> Self {
        if zoom_pow_min > zoom_pow_max {
            panic!("Minimum zoom power exceeds maximum zoom power");
        }

        let zoom_pow = zoom_pow_max.min(zoom_pow_min.max(0));

        ViewTransform {
            offset: 0.0,
            zoom_scale,
            zoom_pow,
            zoom_pow_min,
            zoom_pow_max,
        }
    }

    pub fn offset(&self) -> f64 {
        self.offset
    }

    pub fn set_offset(&mut self, offset: f64) {
        self.offset = offset.floor();
    }

    pub fn zoom_pow(&self) -> i32 {
        self.zoom_pow
    }

    pub fn set_zoom_pow(&mut self, pow: i32) {
        self.zoom_pow = self.zoom_pow_max.min(self.zoom_pow_min.max(pow));
    }

    pub fn zoom(&self) -> f64 {
        (self.zoom_scale).powi(self.zoom_pow)
    }

    pub fn zoom_in(&mut self) {
        self.zoom_pow = self.zoom_pow_max.min(self.zoom_pow + 1);
    }

    pub fn zoom_out(&mut self) {
        self.zoom_pow = self.zoom_pow_min.max(self.zoom_pow - 1);
    }
}

fn main() -> glib::ExitCode {
    let app = Application::builder()
        .application_id("com.loliaintregisterinnodomainname.Quadio")
        .build();

    app.connect_activate(|app| {
        let wav_filter = FileFilter::new();
        wav_filter.set_name(Some("Wave file (.wav)"));
        wav_filter.add_pattern("*.wav");

        let wildcard_filter = FileFilter::new();
        wildcard_filter.set_name(Some("Any file (.*)"));
        wildcard_filter.add_pattern("*");

        let filters = ListStore::with_type(
            Type::OBJECT
        );

        filters.append(&wav_filter);
        filters.append(&wildcard_filter);

        let file_dialog = FileDialog::builder()
            .filters(&filters)
            .build();

        let window = Rc::new(ApplicationWindow::builder()
            .application(app)
            .show_menubar(true)
            .default_width(640)
            .default_height(480)
            .title("Quadio")
            .build());
        
        let window_clone = Rc::clone(&window);

        let import_action = SimpleAction::new(
            "import",
            None
        );

        let canvas = Rc::new(DrawingArea::new());

        let project = Rc::new(RefCell::new(None));
        let waveform = Rc::new(RefCell::new(None));

        {
            let project = Rc::clone(&project);
            let waveform = Rc::clone(&waveform);
            let canvas = Rc::clone(&canvas);

            import_action.connect_activate(move |_, _| {
                let project = Rc::clone(&project);
                let waveform = Rc::clone(&waveform);
                let canvas = Rc::clone(&canvas);

                file_dialog.open(
                    Some(&*window_clone),
                    None::<&Cancellable>,
                    move |file| {
                        let path = file.ok().and_then(|f| f.path());

                        if let Some(path) = path {
                            let file = std::fs::File::open(path).unwrap();
                            let reader = std::io::BufReader::new(file);
                            let qw_reader = core::QWaveReader::new(reader)
                                .unwrap();
                            let proj = core::Project::from_reader(qw_reader)
                                .unwrap();

                            let buffer_width = 2048;

                            let stride = Format::Rgb24.stride_for_width(
                                buffer_width.try_into().unwrap()
                            ).unwrap();

                            let height = 128;

                            let wf = waveform::Waveform::new(
                                proj.samples().to_vec(),
                                1.0/32.0,
                                buffer_width,
                                height,
                                stride,
                                waveform::Theme {
                                    background: u32::from_be_bytes(
                                        [255, 20, 20, 20]
                                    ),
                                    in_range: u32::from_be_bytes(
                                        [255, 255, 230, 0]
                                    ),
                                    rms: u32::from_be_bytes(
                                        [255, 170, 160, 0]
                                    ),
                                },
                                move |pixbuf| {
                                    ImageWrapper {
                                        image: ImageSurface::create_for_data(
                                            pixbuf,
                                            Format::Rgb24,
                                            buffer_width,
                                            height,
                                            stride,
                                        ).unwrap(),
                                    }
                                },
                            );

                            *waveform.borrow_mut() = Some(wf);
                            *project.borrow_mut() = Some(proj);
                            canvas.queue_draw();
                        }
                    }
                );
            });
        }

        app.add_action(&import_action);

        let file_section = Menu::new();
        file_section.append(Some("Import"), Some("app.import"));

        let application_section = Menu::new();
        application_section.append(Some("Quit"), None);

        let file_menu = Menu::new();
        file_menu.append_section(None, &file_section);
        file_menu.append_section(None, &application_section);

        let menu = Menu::new();
        menu.append_submenu(Some("File"), &file_menu);

        app.set_menubar(Some(&menu));

        let last_offset = Rc::new(RefCell::new(0f64));

        let view_transform = Rc::new(RefCell::new(ViewTransform::new(
            -30,
            10,
            1.25,
        )));

        {
            let waveform = Rc::clone(&waveform);
            let view_transform = Rc::clone(&view_transform);

            canvas.set_draw_func(move |_canvas, ctx, width, _height| {
                if let Some(ref mut wf) = &mut *waveform.borrow_mut() {
                    let vt = view_transform.borrow();

                    let window = waveform::Window {
                        offset_px: vt.offset().floor() as i32,
                        zoom: vt.zoom(),
                        width_px: width,
                    };

                    if let waveform::DrawInfo::Image(wrapper) =
                        wf.render(&window)
                    {
                        let wrapper = &*wrapper.borrow();
                        let surface = &wrapper.image;
                        ctx.set_source_surface(surface, 0.0, 0.0).unwrap();
                        ctx.source().set_filter(Filter::Nearest);
                        ctx.paint().unwrap();
                    } else {
                        ctx.set_source_rgb(0.2, 0.2, 0.2);
                        ctx.paint().unwrap();
                    }
                }
            });
        }

        let drag_controller = GestureDrag::builder()
            .button(BUTTON_SECONDARY)
            .touch_only(false)
            .build();

        {
            let last_offset = Rc::clone(&last_offset);
            let view_transform = Rc::clone(&view_transform);
            let canvas = Rc::clone(&canvas);

            drag_controller.connect_drag_update(move |_ctl, x, y| {
                let mut vt = view_transform.borrow_mut();
                vt.set_offset(x.floor() + *last_offset.borrow());
                canvas.queue_draw();
            });
        }

        {
            let last_offset = Rc::clone(&last_offset);
            let view_transform = Rc::clone(&view_transform);

            drag_controller.connect_drag_end(move |_ctl, x, y| {
                *last_offset.borrow_mut() = view_transform.borrow().offset();
            });
        }

        let scroll_controller_flags = EventControllerScrollFlags::VERTICAL
            .union(EventControllerScrollFlags::DISCRETE);

        let scroll_controller = EventControllerScroll::new(
            scroll_controller_flags
        );

        {
            let canvas = Rc::clone(&canvas);
            let waveform = Rc::clone(&waveform);
            let view_transform = Rc::clone(&view_transform);

            scroll_controller.connect_scroll(move |_ctl, _dx, dy| {
                if waveform.borrow().is_some() {
                    let old_z = view_transform.borrow().zoom_pow();

                    if dy < 0.0 {
                        view_transform.borrow_mut().zoom_in();
                    } else if dy > 0.0 {
                        view_transform.borrow_mut().zoom_out();
                    }

                    let new_z = view_transform.borrow().zoom_pow();

                    if new_z != old_z {
                        canvas.queue_draw();
                        Propagation::Stop
                    } else {
                        Propagation::Proceed
                    }
                } else {
                    Propagation::Proceed
                }
            });
        }

        canvas.add_controller(drag_controller);
        canvas.add_controller(scroll_controller);

        window.set_child(Some(&*canvas));

        window.present();
    });

    app.run()
}
