use gtk::{gdk, glib, pango, prelude::*};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::{Duration, Instant},
};

const DURATION: Duration = Duration::from_millis(140);

struct Glyph {
    position: usize,
    start: Instant,
}

pub fn attach(entry: &gtk::Entry) {
    let Some(text_widget) = entry.delegate().and_downcast::<gtk::Text>() else {
        return;
    };
    let glyphs = Rc::new(RefCell::new(Vec::<Glyph>::new()));
    let typing = Rc::new(Cell::new(false));
    let ticking = Rc::new(Cell::new(false));
    let keyboard = gtk::EventControllerKey::new();
    keyboard.set_propagation_phase(gtk::PropagationPhase::Capture);
    let armed = typing.clone();
    keyboard.connect_key_pressed(move |_, key, _, modifiers| {
        if key
            .to_unicode()
            .is_some_and(|character| !character.is_control())
            && !modifiers.intersects(
                gdk::ModifierType::CONTROL_MASK
                    | gdk::ModifierType::ALT_MASK
                    | gdk::ModifierType::SUPER_MASK,
            )
        {
            armed.set(true);
            let armed = armed.clone();
            glib::idle_add_local_once(move || armed.set(false));
        }
        glib::Propagation::Proceed
    });
    entry.add_controller(keyboard);

    let running = glyphs.clone();
    text_widget.connect_delete_text(move |_, start, end| {
        let mut running = running.borrow_mut();
        let start = start.max(0) as usize;
        if end < 0 {
            running.retain(|glyph| glyph.position < start);
            return;
        }
        let end = end as usize;
        running.retain_mut(|glyph| {
            if (start..end).contains(&glyph.position) {
                return false;
            }
            if glyph.position >= end {
                glyph.position -= end - start;
            }
            true
        });
    });

    text_widget.connect_insert_text(move |entry, text, position| {
        let count = text.chars().count();
        let position = (*position).max(0) as usize;
        if !(typing.replace(false) && count == 1 && entry.settings().is_gtk_enable_animations()) {
            glyphs.borrow_mut().clear();
            entry.set_attributes(None);
            return;
        }
        {
            let mut glyphs = glyphs.borrow_mut();
            for glyph in glyphs.iter_mut().filter(|glyph| glyph.position >= position) {
                glyph.position += count;
            }
            glyphs.push(Glyph {
                position,
                start: Instant::now(),
            });
        }
        if ticking.replace(true) {
            return;
        }
        let glyphs = glyphs.clone();
        let ticking = ticking.clone();
        entry.add_tick_callback(move |entry, _| {
            let mut glyphs = glyphs.borrow_mut();
            let now = Instant::now();
            glyphs.retain(|glyph| now.duration_since(glyph.start) < DURATION);
            let attributes = pango::AttrList::new();
            let contents = entry.text();
            let mut offsets: Vec<_> = contents
                .char_indices()
                .map(|(index, _)| index as u32)
                .collect();
            offsets.push(contents.len() as u32);
            for glyph in glyphs.iter() {
                let Some(range) = offsets.get(glyph.position..=glyph.position + 1) else {
                    continue;
                };
                let progress =
                    now.duration_since(glyph.start).as_secs_f64() / DURATION.as_secs_f64();
                let remaining = (1.0 - progress).powi(2);
                let mut rise: pango::Attribute = pango::AttrInt::new_rise(
                    (-2.0 * f64::from(pango::SCALE) * remaining).round() as i32,
                )
                .into();
                let mut opacity: pango::Attribute = pango::AttrInt::new_foreground_alpha(
                    (f64::from(u16::MAX) * (1.0 - 0.75 * remaining)).round() as u16,
                )
                .into();
                for attribute in [&mut rise, &mut opacity] {
                    attribute.set_start_index(range[0]);
                    attribute.set_end_index(range[1]);
                }
                attributes.insert(rise);
                attributes.insert(opacity);
            }
            entry.set_attributes(Some(&attributes));
            if glyphs.is_empty() {
                ticking.set(false);
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
    });
}
