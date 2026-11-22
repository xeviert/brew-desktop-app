//! A least-squares line over a sliding window: 4c's stability fit and 5a's peak rate.

/// Running sums for a least-squares line, so a sliding window costs O(1) per point.
#[derive(Default)]
pub(crate) struct Sums {
    n: f64,
    x: f64,
    y: f64,
    xx: f64,
    xy: f64,
}

impl Sums {
    /// `sign` is `1.0` to add a point, `-1.0` to remove it.
    pub(crate) fn add(&mut self, x: f64, y: f64, sign: f64) {
        self.n += sign;
        self.x += sign * x;
        self.y += sign * y;
        self.xx += sign * x * x;
        self.xy += sign * x * y;
    }

    /// The slope, and the line's value at `at`. Needs two distinct `x`.
    pub(crate) fn fit(&self, at: f64) -> (f64, f64) {
        let sxx = self.xx - self.x * self.x / self.n;
        let sxy = self.xy - self.x * self.y / self.n;
        let slope = sxy / sxx;
        (slope, self.y / self.n + slope * (at - self.x / self.n))
    }
}
