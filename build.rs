use std::env;
use std::fs;
use std::path::Path;

/// Converts the template dolphin PNG (black art + alpha, inherited from the
/// macOS app) into raw ARGB32 pixmaps for StatusNotifierItem: a light variant
/// for the normal state and an orange variant for attention. Keeping the
/// conversion here keeps image crates out of the runtime dependency tree.
fn main() {
    println!("cargo:rerun-if-changed=assets/orca.png");

    let file = fs::File::open("assets/orca.png").expect("assets/orca.png");
    let decoder = png::Decoder::new(std::io::BufReader::new(file));
    let mut reader = decoder.read_info().expect("decode png");
    let mut buffer = vec![0u8; reader.output_buffer_size().expect("buffer size")];
    let info = reader.next_frame(&mut buffer).expect("read frame");
    assert_eq!(info.color_type, png::ColorType::Rgba, "expected RGBA png");
    let rgba = &buffer[..info.buffer_size()];

    let out_dir = env::var("OUT_DIR").unwrap();
    write_variant(&out_dir, "orca-normal.argb", rgba, (0xE8, 0xE8, 0xE8));
    write_variant(&out_dir, "orca-attention.argb", rgba, (0xFF, 0x9F, 0x0A));
    fs::write(
        Path::new(&out_dir).join("icon-size.txt"),
        format!("{}", info.width),
    )
    .unwrap();
}

/// The source art is a template (shape lives in the alpha channel), so recolor
/// every pixel to `tint` and keep the alpha. Output is ARGB32 in network byte
/// order, as the SNI spec requires.
fn write_variant(out_dir: &str, name: &str, rgba: &[u8], tint: (u8, u8, u8)) {
    let mut argb = Vec::with_capacity(rgba.len());
    for pixel in rgba.chunks_exact(4) {
        argb.push(pixel[3]); // A
        argb.push(tint.0); // R
        argb.push(tint.1); // G
        argb.push(tint.2); // B
    }
    fs::write(Path::new(out_dir).join(name), argb).unwrap();
}
