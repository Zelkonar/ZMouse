use tiny_skia::*;

/// Rounded-rectangle path via cubic-bezier corners (kappa ~ 0.5523).
fn rounded_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Path {
    let k = 0.552_284_75 * r;
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
    pb.finish().unwrap()
}

fn fill(pm: &mut Pixmap, path: &Path, color: Color) {
    let mut p = Paint::default();
    p.set_color(color);
    p.anti_alias = true;
    pm.fill_path(path, &p, FillRule::Winding, Transform::identity(), None);
}

fn main() {
    let s = 1024.0;
    let mut pm = Pixmap::new(1024, 1024).unwrap();

    // Background: indigo→violet diagonal gradient in a macOS-style rounded square.
    let bg = rounded_rect(0.0, 0.0, s, s, 229.0);
    let shader = LinearGradient::new(
        Point::from_xy(0.0, 0.0),
        Point::from_xy(s, s),
        vec![
            GradientStop::new(0.0, Color::from_rgba8(0x5b, 0x54, 0xe6, 255)),
            GradientStop::new(1.0, Color::from_rgba8(0x9b, 0x3d, 0xe0, 255)),
        ],
        SpreadMode::Pad,
        Transform::identity(),
    )
    .unwrap();
    let mut bgp = Paint::default();
    bgp.shader = shader;
    bgp.anti_alias = true;
    pm.fill_path(&bg, &bgp, FillRule::Winding, Transform::identity(), None);

    // Mouse body: white rounded shape (narrower/taller = top-view mouse).
    let bw = 400.0;
    let bh = 580.0;
    let bx = (s - bw) / 2.0;
    let by = (s - bh) / 2.0;
    let body = rounded_rect(bx, by, bw, bh, 200.0);
    fill(&mut pm, &body, Color::from_rgba8(255, 255, 255, 255));

    // Scroll wheel: short accent pill near the top center.
    let ww = 28.0;
    let wh = 104.0;
    let wx = (s - ww) / 2.0;
    let wy = by + 74.0;
    let wheel = rounded_rect(wx, wy, ww, wh, 14.0);
    fill(&mut pm, &wheel, Color::from_rgba8(0x6b, 0x4d, 0xe6, 255));

    pm.save_png("icon_1024.png").unwrap();
    println!("wrote icon_1024.png");
}
