// Embeds assets/logo.png as the Windows .exe icon (Explorer, taskbar, Alt-Tab).
use image::codecs::ico::{IcoEncoder, IcoFrame};
use image::{imageops, ExtendedColorType, RgbaImage};
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=assets/logo.png");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let logo = image::open("assets/logo.png").expect("read assets/logo.png").to_rgba8();
    let square = pad_to_square(&logo);

    let frames: Vec<IcoFrame> = [16u32, 24, 32, 48, 64, 128, 256]
        .iter()
        .map(|&s| {
            let img = imageops::resize(&square, s, s, imageops::FilterType::Lanczos3);
            IcoFrame::as_png(img.as_raw(), s, s, ExtendedColorType::Rgba8).expect("encode icon frame")
        })
        .collect();

    let ico_path = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("logo.ico");
    let file = std::fs::File::create(&ico_path).expect("create logo.ico");
    IcoEncoder::new(file).encode_images(&frames).expect("write logo.ico");

    let mut res = winresource::WindowsResource::new();
    res.set_icon(ico_path.to_str().unwrap());
    res.compile().expect("compile Windows resources");
}

fn pad_to_square(img: &RgbaImage) -> RgbaImage {
    let side = img.width().max(img.height());
    let mut out = RgbaImage::new(side, side);
    let x = (side - img.width()) / 2;
    let y = (side - img.height()) / 2;
    imageops::overlay(&mut out, img, x as i64, y as i64);
    out
}
