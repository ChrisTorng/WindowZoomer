use std::env;
use std::fs;
use std::path::PathBuf;

fn make_icon(path: &std::path::Path) -> std::io::Result<()> {
    const W: usize = 64;
    const H: usize = 64;
    let mut pixels = vec![0u8; W * H * 4];

    for y in 0..H {
        for x in 0..W {
            let i = (y * W + x) * 4;
            let dx = x as f32 - 27.0;
            let dy = y as f32 - 25.0;
            let r2 = dx * dx + dy * dy;
            let ring = (15.0f32.powi(2)..=21.0f32.powi(2)).contains(&r2);
            let handle = {
                let a = x as i32 - y as i32;
                x >= 39 && y >= 37 && x <= 58 && y <= 57 && (-3..=5).contains(&a)
            };
            if ring || handle {
                pixels[i] = 245;     // B
                pixels[i + 1] = 245; // G
                pixels[i + 2] = 245; // R
                pixels[i + 3] = 255; // A
            } else if r2 < 15.0f32.powi(2) {
                pixels[i] = 160;
                pixels[i + 1] = 95;
                pixels[i + 2] = 35;
                pixels[i + 3] = 255;
            } else {
                pixels[i] = 0;
                pixels[i + 1] = 0;
                pixels[i + 2] = 0;
                pixels[i + 3] = 0;
            }
        }
    }

    let xor_size = W * H * 4;
    let mask_stride = ((W + 31) / 32) * 4;
    let mask_size = mask_stride * H;
    let image_size = 40 + xor_size + mask_size;

    let mut out = Vec::with_capacity(6 + 16 + image_size);
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());

    out.push(W as u8);
    out.push(H as u8);
    out.push(0);
    out.push(0);
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&(image_size as u32).to_le_bytes());
    out.extend_from_slice(&22u32.to_le_bytes());

    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(W as i32).to_le_bytes());
    out.extend_from_slice(&((H * 2) as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(xor_size as u32).to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());

    for y in (0..H).rev() {
        let row = &pixels[y * W * 4..(y + 1) * W * 4];
        out.extend_from_slice(row);
    }
    out.resize(out.len() + mask_size, 0);
    fs::write(path, out)
}

fn main() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let icon = out_dir.join("windowzoomer.ico");
    make_icon(&icon).unwrap();

    let mut res = winresource::WindowsResource::new();
    res.set_icon(icon.to_str().unwrap())
        .set("FileDescription", "WindowZoomer")
        .set("ProductName", "WindowZoomer")
        .set("InternalName", "WindowZoomer")
        .set("OriginalFilename", "WindowZoomer.exe")
        .set("CompanyName", "ChrisTorng");
    res.compile().unwrap();
}
