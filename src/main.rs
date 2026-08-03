mod anim;
mod app;
mod archive;
mod cache;
mod colo_thumb;
mod decode;
mod dls;
mod format_color;
mod git;
mod image_types;
mod libxmp;
mod palettes_builtin;
mod rating;
mod ratings;
mod sauce;
mod scale;
mod sfz;
mod sixteen;
mod soundfont;
mod steam;
mod thumb;
mod video;
mod viewdb;
mod xi;
mod youtube;

use eframe::egui;

fn main() -> Result<(), eframe::Error> {
    let cli = app::CliArgs::parse();

    // Headless render-to-file mode (`--render`): convert art to image files and exit,
    // without ever opening a window. Runs fine over SSH / in a batch script.
    if cli.is_render() {
        std::process::exit(app::run_render(&cli));
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 760.0])
            .with_min_inner_size([640.0, 420.0])
            .with_title("pixelview")
            // Wayland compositors key the task-switcher icon off the app_id (matched
            // against a pixelview.desktop). `with_icon` covers X11 / other backends.
            .with_app_id("pixelview")
            .with_icon(app_icon()),
        ..Default::default()
    };

    eframe::run_native(
        "pixelview",
        options,
        Box::new(move |cc| Ok(Box::new(app::PixelView::new(cc, cli)))),
    )
}

/// A generated window icon: a 4×4 grid of bright "thumbnails" on a dark field —
/// a nod to the thumbnail grid this whole app is built around.
fn app_icon() -> egui::IconData {
    const S: usize = 64;
    const PAL: [[u8; 3]; 16] = [
        [231, 76, 60],
        [46, 204, 113],
        [52, 152, 219],
        [241, 196, 15],
        [155, 89, 182],
        [26, 188, 156],
        [230, 126, 34],
        [236, 240, 241],
        [52, 73, 94],
        [243, 156, 18],
        [142, 68, 173],
        [22, 160, 133],
        [192, 57, 43],
        [41, 128, 185],
        [39, 174, 96],
        [211, 84, 0],
    ];
    let mut rgba = vec![0u8; S * S * 4];
    for px in rgba.chunks_exact_mut(4) {
        px.copy_from_slice(&[24, 26, 32, 255]); // dark background
    }
    let (margin, gap, cells) = (6usize, 3usize, 4usize);
    let cell = (S - 2 * margin - (cells - 1) * gap) / cells;
    for cy in 0..cells {
        for cx in 0..cells {
            let color = PAL[cy * cells + cx];
            let (x0, y0) = (margin + cx * (cell + gap), margin + cy * (cell + gap));
            for y in y0..y0 + cell {
                for x in x0..x0 + cell {
                    let o = (y * S + x) * 4;
                    rgba[o..o + 3].copy_from_slice(&color);
                    rgba[o + 3] = 255;
                }
            }
        }
    }
    egui::IconData {
        rgba,
        width: S as u32,
        height: S as u32,
    }
}
