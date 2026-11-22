//! Génère `app/icon.ico` (16→256 px, PNG intégrés) et `ui/ui/logo.png` depuis `logo/emunest.svg`.
//! Usage : cargo run --release (depuis tools/make-icon)
use resvg::{tiny_skia, usvg};

fn render(tree: &usvg::Tree, size: u32) -> Vec<u8> {
    let mut pix = tiny_skia::Pixmap::new(size, size).unwrap();
    let scale = size as f32 / tree.size().width();
    resvg::render(tree, tiny_skia::Transform::from_scale(scale, scale), &mut pix.as_mut());
    pix.encode_png().unwrap()
}

fn main() {
    let root = std::path::Path::new("../..");
    let svg = std::fs::read(root.join("logo/emunest.svg")).expect("logo/emunest.svg");
    let tree = usvg::Tree::from_data(&svg, &usvg::Options::default()).expect("svg invalide");

    let sizes = [16u32, 24, 32, 48, 64, 128, 256];
    let pngs: Vec<Vec<u8>> = sizes.iter().map(|&s| render(&tree, s)).collect();

    // ICO : en-tête, un répertoire d'entrées, puis les PNG
    let mut ico = Vec::new();
    ico.extend_from_slice(&[0, 0, 1, 0]);
    ico.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (s, png) in sizes.iter().zip(&pngs) {
        let dim = if *s >= 256 { 0 } else { *s as u8 };
        ico.extend_from_slice(&[dim, dim, 0, 0, 1, 0, 32, 0]);
        ico.extend_from_slice(&(png.len() as u32).to_le_bytes());
        ico.extend_from_slice(&offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for png in &pngs {
        ico.extend_from_slice(png);
    }
    std::fs::write(root.join("app/icon.ico"), ico).unwrap();
    std::fs::write(root.join("ui/ui/logo.png"), render(&tree, 256)).unwrap();
    println!("ok : app/icon.ico et ui/ui/logo.png");
}
