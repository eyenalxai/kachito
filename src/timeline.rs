//! The selection model under the timeline: in and out points, zoom and the visible window.

pub const MIN_FRAME: f64 = 1.0 / 60.0;
const MAX_ZOOM: f64 = 32.0;

#[derive(Clone, Debug, PartialEq)]
pub struct Timeline {
    pub duration: f64,
    pub position: f64,
    pub in_point: f64,
    pub out_point: f64,
    pub zoom: f64,
    pub view_start: f64,
}

impl Default for Timeline {
    fn default() -> Self {
        Self::new()
    }
}

impl Timeline {
    pub fn new() -> Self {
        Self {
            duration: 0.0,
            position: 0.0,
            in_point: 0.0,
            out_point: 0.0,
            zoom: 1.0,
            view_start: 0.0,
        }
    }

    pub fn set_duration(&mut self, duration: f64) {
        if !duration.is_finite() || duration <= 0.0 {
            self.duration = 0.0;
            self.position = 0.0;
            self.in_point = 0.0;
            self.out_point = 0.0;
            self.view_start = 0.0;
            return;
        }
        let first = self.duration <= 0.0;
        self.duration = duration;
        if first {
            self.in_point = 0.0;
            self.out_point = duration;
        } else {
            self.out_point = self.out_point.min(duration);
            self.in_point = self.in_point.min((self.out_point - MIN_FRAME).max(0.0));
        }
        self.position = self.position.clamp(0.0, duration);
        self.view_start = self
            .view_start
            .clamp(0.0, (duration - self.visible_span()).max(0.0));
    }

    pub fn visible_span(&self) -> f64 {
        if self.duration <= 0.0 {
            0.0
        } else {
            self.duration / self.zoom
        }
    }

    pub fn view_end(&self) -> f64 {
        (self.view_start + self.visible_span()).min(self.duration)
    }

    pub fn set_position(&mut self, position: f64) {
        self.position = position.clamp(0.0, self.duration);
        self.follow();
    }

    pub fn set_in(&mut self, time: f64) {
        let limit = (self.out_point - MIN_FRAME).max(0.0);
        self.in_point = time.clamp(0.0, limit);
    }

    pub fn set_out(&mut self, time: f64) {
        let floor = (self.in_point + MIN_FRAME).min(self.duration);
        self.out_point = time.clamp(floor, self.duration);
    }

    pub fn selection(&self) -> (f64, f64) {
        (self.in_point, self.out_point)
    }

    pub fn selection_duration(&self) -> f64 {
        (self.out_point - self.in_point).max(0.0)
    }

    pub fn set_zoom(&mut self, zoom: f64) {
        if !zoom.is_finite() {
            return;
        }
        let anchor = if self.duration <= 0.0 {
            0.0
        } else {
            self.position.clamp(self.view_start, self.view_end())
        };
        let fraction = if self.visible_span() > 0.0 {
            (anchor - self.view_start) / self.visible_span()
        } else {
            0.5
        };
        self.zoom = zoom.clamp(1.0, MAX_ZOOM);
        let span = self.visible_span();
        self.view_start = (anchor - fraction * span).clamp(0.0, (self.duration - span).max(0.0));
    }

    pub fn zoom_in(&mut self) {
        self.set_zoom(self.zoom * 1.5);
    }

    pub fn zoom_out(&mut self) {
        self.set_zoom(self.zoom / 1.5);
    }

    pub fn time_at_fraction(&self, fraction: f64) -> f64 {
        self.view_start + fraction.clamp(0.0, 1.0) * self.visible_span()
    }

    pub fn fraction_of(&self, time: f64) -> f64 {
        let span = self.visible_span();
        if span <= 0.0 {
            0.0
        } else {
            ((time - self.view_start) / span).clamp(0.0, 1.0)
        }
    }

    fn follow(&mut self) {
        let span = self.visible_span();
        if span <= 0.0 {
            self.view_start = 0.0;
            return;
        }
        if self.position < self.view_start || self.position > self.view_start + span {
            self.view_start =
                (self.position - span * 0.5).clamp(0.0, (self.duration - span).max(0.0));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_timeline_selects_the_whole_file() {
        let mut timeline = Timeline::new();
        timeline.set_duration(100.0);
        assert_eq!(timeline.selection(), (0.0, 100.0));
        assert_eq!(timeline.selection_duration(), 100.0);
    }

    #[test]
    fn in_and_out_clamp_and_keep_one_frame() {
        let mut timeline = Timeline::new();
        timeline.set_duration(10.0);
        timeline.set_in(3.0);
        timeline.set_out(8.0);
        assert_eq!(timeline.selection(), (3.0, 8.0));
        timeline.set_in(9.0);
        assert!(timeline.in_point <= 8.0 - MIN_FRAME);
        timeline.set_out(1.0);
        assert!(timeline.out_point >= timeline.in_point + MIN_FRAME);
        timeline.set_in(-4.0);
        assert_eq!(timeline.in_point, 0.0);
    }

    #[test]
    fn zoom_keeps_the_position_in_view_and_respects_bounds() {
        let mut timeline = Timeline::new();
        timeline.set_duration(100.0);
        timeline.set_position(50.0);
        timeline.zoom_in();
        assert!(timeline.visible_span() < 100.0);
        assert!(timeline.position >= timeline.view_start);
        assert!(timeline.position <= timeline.view_end());
        timeline.set_zoom(1000.0);
        assert_eq!(timeline.zoom, MAX_ZOOM);
        timeline.set_zoom(0.1);
        assert_eq!(timeline.zoom, 1.0);
    }

    #[test]
    fn fractions_round_trip_inside_the_visible_window() {
        let mut timeline = Timeline::new();
        timeline.set_duration(100.0);
        timeline.set_position(50.0);
        timeline.set_zoom(4.0);
        let time = timeline.time_at_fraction(0.25);
        assert!((timeline.fraction_of(time) - 0.25).abs() < 1e-9);
    }
}
