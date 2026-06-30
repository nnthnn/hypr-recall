use gdk_pixbuf::PixbufLoader;
use gtk4::{
    gdk, glib, prelude::*, Application, ApplicationWindow, Box as GtkBox, CssProvider, Label,
    Orientation, Picture,
};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const LOGO: &[u8] = include_bytes!("../../assets/logo-transparent.png");

const CSS: &str = "
    window { background-color: rgba(0, 0, 0, 0.65); }
    .logo { animation: spin 1.8s linear infinite; }
    @keyframes spin {
        from { transform: rotate(0deg); }
        to   { transform: rotate(360deg); }
    }
    .progress {
        color: rgba(255, 255, 255, 0.85);
        font-size: 15px;
        margin-top: 20px;
        font-weight: 500;
        letter-spacing: 1px;
    }
";

fn main() {
    let app = Application::builder()
        .application_id("io.github.nnthnn.hypr-recall.overlay")
        .build();

    app.connect_activate(build_ui);
    app.run_with_args::<String>(&[]);
}

fn build_ui(app: &Application) {
    let shared_msg: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let shared_for_thread = Arc::clone(&shared_msg);

    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::stdin().lock().lines() {
            match line {
                Ok(msg) => {
                    if let Ok(mut guard) = shared_for_thread.lock() {
                        *guard = Some(msg);
                    }
                }
                Err(_) => break,
            }
        }
    });

    let window = ApplicationWindow::new(app);

    window.init_layer_shell();
    window.set_layer(Layer::Overlay);
    window.set_anchor(Edge::Top, true);
    window.set_anchor(Edge::Bottom, true);
    window.set_anchor(Edge::Left, true);
    window.set_anchor(Edge::Right, true);
    window.set_keyboard_mode(KeyboardMode::None);
    window.set_exclusive_zone(-1);

    let css = CssProvider::new();
    css.load_from_data(CSS);
    gtk4::style_context_add_provider_for_display(
        &gdk::Display::default().expect("no display"),
        &css,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    let loader = PixbufLoader::new();
    loader.write(LOGO).expect("write logo bytes");
    loader.close().expect("close pixbuf loader");
    let pixbuf = loader.pixbuf().expect("logo pixbuf");
    let scaled = pixbuf
        .scale_simple(112, 112, gdk_pixbuf::InterpType::Bilinear)
        .expect("scale pixbuf");
    let texture = gdk::Texture::for_pixbuf(&scaled);

    let picture = Picture::for_paintable(&texture);
    picture.set_can_shrink(false);
    picture.add_css_class("logo");

    let label = Label::new(None);
    label.add_css_class("progress");

    let center = GtkBox::new(Orientation::Vertical, 0);
    center.set_hexpand(true);
    center.set_vexpand(true);
    center.set_halign(gtk4::Align::Center);
    center.set_valign(gtk4::Align::Center);
    center.append(&picture);
    center.append(&label);

    window.set_child(Some(&center));
    window.present();

    glib::timeout_add_local(Duration::from_millis(100), move || {
        if let Ok(mut guard) = shared_msg.try_lock() {
            if let Some(msg) = guard.take() {
                label.set_text(&msg);
            }
        }
        glib::ControlFlow::Continue
    });
}
