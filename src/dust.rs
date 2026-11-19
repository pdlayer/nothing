use gtk::{cairo, gdk_pixbuf::InterpType, glib, prelude::*};
use serde::Deserialize;
use std::{
    cell::Cell,
    rc::Rc,
    time::{Duration, Instant},
};
use webkit6::{SnapshotOptions, SnapshotRegion, WebView, prelude::*};

#[derive(Deserialize)]
struct Clip {
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Selection {
    pub rule: String,
    clip: Clip,
    document_time: f64,
    from_left: bool,
    reduced: bool,
}

struct Grain {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    start: f64,
    span: f64,
    wind: f64,
    lift: f64,
    turbulence: f64,
    color: [f64; 3],
}

pub async fn animate(view: &WebView, selection: &Selection) {
    if selection.reduced || !gtk::prelude::WidgetExt::settings(view).is_gtk_enable_animations() {
        return;
    }
    let Some(overlay) = view
        .ancestor(gtk::Overlay::static_type())
        .and_downcast::<gtk::Overlay>()
    else {
        return;
    };
    let Ok(texture) = view
        .snapshot_future(SnapshotRegion::Visible, SnapshotOptions::NONE)
        .await
    else {
        return;
    };
    let Some(bounds) = view.compute_bounds(&overlay) else {
        return;
    };
    let clip = &selection.clip;
    let left = (clip.left * texture.width() as f64).floor() as i32;
    let top = (clip.top * texture.height() as f64).floor() as i32;
    let right = (clip.right * texture.width() as f64).ceil() as i32;
    let bottom = (clip.bottom * texture.height() as f64).ceil() as i32;
    if left < 0
        || top < 0
        || right > texture.width()
        || bottom > texture.height()
        || right <= left
        || bottom <= top
    {
        return;
    }
    let Some(bitmap) = gtk::gdk::pixbuf_get_from_texture(&texture) else {
        return;
    };
    let crop = bitmap.new_subpixbuf(left, top, right - left, bottom - top);
    let w = (f64::from(bounds.width()) * (clip.right - clip.left)).round() as i32;
    let h = (f64::from(bounds.height()) * (clip.bottom - clip.top)).round() as i32;
    if w < 1 || h < 1 {
        return;
    }
    let Some(image) = crop.scale_simple(w, h, InterpType::Bilinear) else {
        return;
    };
    let origin = (
        f64::from(bounds.x()) + clip.left * f64::from(bounds.width()),
        f64::from(bounds.y()) + clip.top * f64::from(bounds.height()),
    );
    let step = ((f64::from(w) * f64::from(h) / 6500.).sqrt().ceil() as usize).max(2);
    let bytes = image.read_pixel_bytes();
    let mut grains = Vec::new();
    let mut seed = 179u32;
    let mut noise = || {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        f64::from(seed) / f64::from(u32::MAX)
    };
    for y in (0..h as usize).step_by(step) {
        for x in (0..w as usize).step_by(step) {
            let width = step.min(w as usize - x);
            let height = step.min(h as usize - y);
            let i = (y + height / 2) * image.rowstride() as usize
                + (x + width / 2) * image.n_channels() as usize;
            let distance = if selection.from_left {
                x as f64 / w as f64
            } else {
                1. - x as f64 / w as f64
            };
            grains.push(Grain {
                x: x as f64,
                y: y as f64,
                width: width as f64,
                height: height as f64,
                start: (distance * 0.46 + noise() * 0.14 + 0.035 * (y as f64 * 0.045).sin())
                    .max(0.),
                span: 0.25 + noise() * 0.13,
                wind: 55. + noise() * 155.,
                lift: 15. + noise() * 85.,
                turbulence: noise() * 12. - 6.,
                color: [bytes[i], bytes[i + 1], bytes[i + 2]].map(|v| f64::from(v) / 255.),
            });
        }
    }
    // Only the native overlay receives screenshot pixels. The page receives no image data.
    let Some(manager) = view.user_content_manager() else {
        return;
    };
    let document = view
        .evaluate_javascript_future(
            "performance.timeOrigin",
            Some("nothing-browser-picker"),
            None,
        )
        .await;
    if !document.is_ok_and(|value| value.to_double() == selection.document_time) {
        return;
    }
    let mask = webkit6::UserStyleSheet::new(
        &format!("{} {{ opacity: 0 !important; }}", selection.rule),
        webkit6::UserContentInjectedFrames::TopFrame,
        webkit6::UserStyleLevel::User,
        &[],
        &[],
    );
    let layer = gtk::DrawingArea::builder()
        .can_target(false)
        .focusable(false)
        .build();
    let progress = Rc::new(Cell::new(0.));
    let current = progress.clone();
    let direction = if selection.from_left { 1. } else { -1. };
    layer.set_draw_func(move |_, cr, _, _| {
        let time = current.get();
        cr.translate(origin.0, origin.1);
        cr.set_antialias(cairo::Antialias::None);
        // Paint the untouched portion at full resolution until each cell breaks away.
        let _ = cr.save();
        for g in &grains {
            if time <= g.start {
                cr.rectangle(g.x, g.y, g.width, g.height);
            }
        }
        cr.clip();
        cr.set_source_pixbuf(&image, 0., 0.);
        let _ = cr.paint();
        let _ = cr.restore();
        for g in &grains {
            let age = (time - g.start) / g.span;
            if !(0.0..1.0).contains(&age) {
                continue;
            }
            let travel = age * age;
            let x = g.x + direction * g.wind * travel;
            let y = g.y - g.lift * age + g.turbulence * (age * 6.).sin();
            let size = (1. - age).powf(1.4);
            cr.set_source_rgba(g.color[0], g.color[1], g.color[2], 1. - age * age);
            cr.rectangle(x, y, (g.width * size).max(0.6), (g.height * size).max(0.6));
            let _ = cr.fill();
        }
    });
    overlay.add_overlay(&layer);
    manager.add_style_sheet(&mask);
    let duration = Duration::from_millis(900);
    let start = Instant::now();
    let weak = view.downgrade();
    layer.add_tick_callback(move |layer, _| {
        let elapsed = start.elapsed();
        if elapsed >= duration || !weak.upgrade().is_some_and(|v| v.is_mapped()) {
            layer.set_visible(false);
            return glib::ControlFlow::Break;
        }
        progress.set(elapsed.as_secs_f64() / duration.as_secs_f64());
        layer.queue_draw();
        glib::ControlFlow::Continue
    });
    glib::timeout_future(duration).await;
    overlay.remove_overlay(&layer);
    manager.remove_style_sheet(&mask);
}
