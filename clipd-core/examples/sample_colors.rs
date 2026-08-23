//! Pull exact colours out of a design image.
//!
//! Eyeballing a palette from a screenshot does not work — this reads the actual
//! pixels so the themes can be built from measured values.
//!
//! Usage:
//!   cargo run -p clipd-core --example sample_colors -- <image> grid <cols> <rows>
//!   cargo run -p clipd-core --example sample_colors -- <image> rect <x> <y> <w> <h>

use std::collections::HashMap;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("usage: <image> grid <cols> <rows> | <image> rect <x> <y> <w> <h>");
        std::process::exit(2);
    }

    let img = image::open(&args[0])
        .unwrap_or_else(|e| panic!("couldn't open {}: {e}", args[0]))
        .to_rgb8();
    let (w, h) = img.dimensions();
    eprintln!("{} is {w}x{h}", args[0]);

    match args[1].as_str() {
        "grid" => {
            let cols: u32 = args[2].parse().expect("cols");
            let rows: u32 = args[3].parse().expect("rows");
            // Average each cell — enough to see where the panels are.
            for row in 0..rows {
                let mut line = String::new();
                for col in 0..cols {
                    let (x0, y0) = (col * w / cols, row * h / rows);
                    let (x1, y1) = ((col + 1) * w / cols, (row + 1) * h / rows);
                    let (mut r, mut g, mut b, mut n) = (0u64, 0u64, 0u64, 0u64);
                    for y in y0..y1 {
                        for x in x0..x1 {
                            let p = img.get_pixel(x, y).0;
                            r += p[0] as u64;
                            g += p[1] as u64;
                            b += p[2] as u64;
                            n += 1;
                        }
                    }
                    if n == 0 {
                        continue;
                    }
                    line.push_str(&format!("{:02x}{:02x}{:02x} ", r / n, g / n, b / n));
                }
                println!("row {row:2}: {line}");
            }
        }
        "rect" => {
            let x0: u32 = args[2].parse().expect("x");
            let y0: u32 = args[3].parse().expect("y");
            let rw: u32 = args[4].parse().expect("w");
            let rh: u32 = args[5].parse().expect("h");

            let mut counts: HashMap<[u8; 3], u32> = HashMap::new();
            for y in y0..(y0 + rh).min(h) {
                for x in x0..(x0 + rw).min(w) {
                    *counts.entry(img.get_pixel(x, y).0).or_insert(0) += 1;
                }
            }
            let total: u32 = counts.values().sum();
            let mut top: Vec<_> = counts.into_iter().collect();
            top.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
            println!("rect {x0},{y0} {rw}x{rh} — {total} px");
            for (c, n) in top.into_iter().take(12) {
                println!(
                    "  #{:02x}{:02x}{:02x}  Rgb({}, {}, {})  {:.1}%",
                    c[0],
                    c[1],
                    c[2],
                    c[0],
                    c[1],
                    c[2],
                    100.0 * n as f32 / total as f32
                );
            }
        }
        // Accents and text are small features — a few hundred pixels against a
        // background of tens of thousands — so the dominant colour never finds
        // them. These are the outliers, which is exactly what they are.
        "extreme" => {
            let x0: u32 = args[2].parse().expect("x");
            let y0: u32 = args[3].parse().expect("y");
            let rw: u32 = args[4].parse().expect("w");
            let rh: u32 = args[5].parse().expect("h");

            let sat = |p: [u8; 3]| {
                p[0].max(p[1]).max(p[2]) as i32 - p[0].min(p[1]).min(p[2]) as i32
            };
            let lum = |p: [u8; 3]| p[0] as i32 + p[1] as i32 + p[2] as i32;

            let mut saturated: Vec<[u8; 3]> = Vec::new();
            let mut brightest = [0u8; 3];
            let mut darkest = [255u8; 3];
            for y in y0..(y0 + rh).min(h) {
                for x in x0..(x0 + rw).min(w) {
                    let p = img.get_pixel(x, y).0;
                    saturated.push(p);
                    if lum(p) > lum(brightest) {
                        brightest = p;
                    }
                    if lum(p) < lum(darkest) {
                        darkest = p;
                    }
                }
            }
            saturated.sort_by_key(|p| std::cmp::Reverse(sat(*p)));

            let show = |label: &str, c: [u8; 3]| {
                println!(
                    "  {label:12} #{:02x}{:02x}{:02x}  Rgb({}, {}, {})  sat {}",
                    c[0],
                    c[1],
                    c[2],
                    c[0],
                    c[1],
                    c[2],
                    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
                );
            };
            println!("rect {x0},{y0} {rw}x{rh}");
            show("brightest", brightest);
            show("darkest", darkest);
            // Several of the top saturated pixels, so antialiased edges don't
            // pass themselves off as the real accent.
            for (i, c) in saturated.into_iter().take(4).enumerate() {
                show(&format!("sat #{}", i + 1), c);
            }
        }
        other => {
            eprintln!("unknown mode {other}");
            std::process::exit(2);
        }
    }
}
