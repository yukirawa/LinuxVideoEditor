use serde::{Deserialize, Serialize};
use std::fmt;
use std::ops::{Add, Sub};

/// Project time stored as integer microseconds to avoid cumulative floating-point drift.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct TimelineTime(i64);

impl TimelineTime {
    pub const ZERO: Self = Self(0);
    pub const MICROS_PER_SECOND: i64 = 1_000_000;

    pub const fn from_micros(micros: i64) -> Self {
        Self(micros)
    }

    pub fn from_seconds(seconds: f64) -> Self {
        if !seconds.is_finite() {
            return Self::ZERO;
        }
        Self((seconds.max(0.0) * Self::MICROS_PER_SECOND as f64).round() as i64)
    }

    pub const fn as_micros(self) -> i64 {
        self.0
    }

    pub fn as_seconds(self) -> f64 {
        self.0 as f64 / Self::MICROS_PER_SECOND as f64
    }

    pub const fn max(self, other: Self) -> Self {
        if self.0 >= other.0 { self } else { other }
    }
}

impl Add for TimelineTime {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self(self.0.saturating_add(rhs.0))
    }
}

impl Sub for TimelineTime {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        Self(self.0.saturating_sub(rhs.0))
    }
}

impl fmt::Display for TimelineTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let total_ms = self.0.max(0) / 1_000;
        let ms = total_ms % 1_000;
        let total_seconds = total_ms / 1_000;
        let seconds = total_seconds % 60;
        let minutes = (total_seconds / 60) % 60;
        let hours = total_seconds / 3_600;
        write!(f, "{hours:02}:{minutes:02}:{seconds:02}.{ms:03}")
    }
}
