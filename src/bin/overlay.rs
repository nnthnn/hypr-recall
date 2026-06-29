use gdk_pixbuf::PixbufLoader;
use gtk4::{
    gdk, prelude::*, Application, ApplicationWindow, Box as GtkBox, CssProvider, Orientation,
    Picture,
};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

const LOGO: &[u8] = include_bytes!("../../assets/logo-transparent.png");

const CSS: &str = "
    window { background-color: rgba(0, 0, 0, 0.65); }
    .logo { animation: spin 1.8s linear infinite; }
    @keyframes spin {
        from { transform: rotate(0deg); }
        to   { transform: rotate(360deg); }
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
    css.load_from_data(CSS); // takes &str in gtk4 0.11.x
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

    let center = GtkBox::new(Orientation::Vertical, 0);
    center.set_hexpand(true);
    center.set_vexpand(true);
    center.set_halign(gtk4::Align::Center);
    center.set_valign(gtk4::Align::Center);
    center.append(&picture);

    window.set_child(Some(&center));
    window.present();
}
