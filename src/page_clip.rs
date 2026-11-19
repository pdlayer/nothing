// The dock changes the clip, while WebKit keeps the same viewport throughout the slide.
use gtk::{glib, prelude::*, subclass::prelude::*};

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct PageClip;

    #[glib::object_subclass]
    impl ObjectSubclass for PageClip {
        const NAME: &'static str = "NothingPageClip";
        type Type = super::PageClip;
        type ParentType = gtk::Widget;
    }
    impl ObjectImpl for PageClip {
        fn dispose(&self) {
            if let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }
    impl WidgetImpl for PageClip {
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            self.obj()
                .first_child()
                .unwrap()
                .measure(orientation, for_size)
        }
        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            let widget = self.obj();
            let edge_height = widget
                .next_sibling()
                .and_then(|dock| dock.first_child())
                .map_or(0, |edge| edge.measure(gtk::Orientation::Vertical, width).1);
            let full = widget
                .parent()
                .map_or(height, |frame| frame.height() - edge_height);
            widget
                .first_child()
                .unwrap()
                .allocate(width, full.max(height), baseline, None);
        }
    }
}
glib::wrapper! {
    pub struct PageClip(ObjectSubclass<imp::PageClip>) @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}
impl PageClip {
    pub fn new(child: &gtk::Overlay) -> Self {
        let widget: Self = glib::Object::new();
        widget.set_overflow(gtk::Overflow::Hidden);
        widget.set_vexpand(true);
        child.set_parent(&widget);
        widget
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    #[ignore = "requires a graphical session"]
    fn sliding_dock_keeps_content_height() {
        gtk::init().unwrap();
        let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let layout = gtk::Overlay::new();
        let view = gtk::DrawingArea::new();
        layout.set_child(Some(&view));
        let clip = PageClip::new(&layout);
        frame.append(&clip);
        let dock = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let edge = gtk::Box::new(gtk::Orientation::Vertical, 0);
        edge.set_height_request(9);
        dock.append(&edge);
        let panel = gtk::Revealer::builder()
            .transition_duration(160)
            .transition_type(gtk::RevealerTransitionType::SlideUp)
            .build();
        let bar = gtk::Label::new(Some("Address"));
        bar.set_height_request(40);
        panel.set_child(Some(&bar));
        dock.append(&panel);
        frame.append(&dock);
        let window = gtk::Window::builder()
            .default_width(480)
            .default_height(320)
            .child(&frame)
            .build();
        window.present();
        glib::MainContext::default().block_on(async {
            glib::timeout_future(Duration::from_millis(350)).await;
            let initial = view.height();
            assert!(initial > 100);
            let initial_clip = clip.height();
            for open in [true, false] {
                panel.set_reveal_child(open);
                for _ in 0..18 {
                    glib::timeout_future(Duration::from_millis(16)).await;
                    assert_eq!(view.height(), initial);
                }
                assert_eq!(clip.height(), initial_clip - if open { 40 } else { 0 });
            }
        });
        window.close();
    }
}
