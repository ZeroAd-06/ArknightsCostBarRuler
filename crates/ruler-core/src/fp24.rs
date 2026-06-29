#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd)]
pub struct Fp24(i64);

impl Fp24 {
    pub(crate) const SCALE_RAW: i64 = 1_i64 << 24;
    pub(crate) const EPSILON_RAW: i64 = 167;
    #[cfg(test)]
    pub(crate) const REQUIRED_ONE: Self = Self(Self::SCALE_RAW);
    pub(crate) const NORMAL_SPEED: Self = Self(Self::SCALE_RAW / 30);
    pub(crate) const NEGATIVE_SPEED: Self = Self(Self::SCALE_RAW / 60);

    pub(crate) const fn from_raw(raw: i64) -> Self {
        Self(raw)
    }

    pub(crate) const fn raw(self) -> i64 {
        self.0
    }

    pub(crate) const fn saturating_add(self, rhs: Self) -> Self {
        Self(self.0.saturating_add(rhs.0))
    }

    pub(crate) const fn saturating_sub(self, rhs: Self) -> Self {
        Self(self.0.saturating_sub(rhs.0))
    }

    #[cfg(test)]
    pub(crate) const fn saturating_mul(self, rhs: i64) -> Self {
        Self(self.0.saturating_mul(rhs))
    }

    pub(crate) const fn is_positive(self) -> bool {
        self.0 > 0
    }

    #[cfg(test)]
    pub(crate) fn required_from_frames_per_cost(frames: i32) -> Self {
        Self::required_from_frame_ratio(frames, 1)
    }

    pub(crate) fn required_from_frame_ratio(total_frames: i32, cycle_count: usize) -> Self {
        if total_frames <= 0 || cycle_count == 0 {
            return Self(0);
        }
        let denominator = 30_i64.saturating_mul(cycle_count as i64);
        Self((total_frames as i64).saturating_mul(Self::SCALE_RAW) / denominator)
    }

    pub(crate) fn phase(self, required: Self) -> f64 {
        if !required.is_positive() {
            return 0.0;
        }
        (self.0 as f64 / required.0 as f64).clamp(0.0, 1.0)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Fp24Advance {
    pub(crate) recovered_cost: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Fp24CostTiming {
    required: Fp24,
    accumulator: Fp24,
    frames_since_cycle_start: i32,
    cycle_index: u64,
}

impl Fp24CostTiming {
    pub(crate) const fn new(required: Fp24) -> Self {
        Self {
            required,
            accumulator: Fp24::from_raw(0),
            frames_since_cycle_start: 0,
            cycle_index: 0,
        }
    }

    pub(crate) const fn required(self) -> Fp24 {
        self.required
    }

    pub(crate) const fn accumulator(self) -> Fp24 {
        self.accumulator
    }

    pub(crate) const fn frames_since_cycle_start(self) -> i32 {
        self.frames_since_cycle_start
    }

    pub(crate) const fn cycle_index(self) -> u64 {
        self.cycle_index
    }

    pub(crate) fn phase(self) -> f64 {
        self.accumulator.phase(self.required)
    }

    pub(crate) fn advance_one_frame(&mut self, speed: Fp24) -> Fp24Advance {
        self.accumulator = self.accumulator.saturating_add(speed);
        self.frames_since_cycle_start = self.frames_since_cycle_start.saturating_add(1);

        let recovered_cost =
            self.accumulator.raw().saturating_add(Fp24::EPSILON_RAW) >= self.required.raw();
        if recovered_cost {
            self.accumulator = self.accumulator.saturating_sub(self.required);
            self.frames_since_cycle_start = 0;
            self.cycle_index = self.cycle_index.saturating_add(1);
        }

        Fp24Advance { recovered_cost }
    }

    pub(crate) fn advanced(mut self, frames: i32, speed: Fp24) -> Self {
        for _ in 0..frames.max(0) {
            self.advance_one_frame(speed);
        }
        self
    }

    pub(crate) fn frames_until_next_cost(self, speed: Fp24) -> i32 {
        let mut timing = self;
        for frames in 1..=10_000 {
            if timing.advance_one_frame(speed).recovered_cost {
                return frames;
            }
        }
        0
    }

    pub(crate) fn total_frames_in_cycle(self, speed: Fp24) -> i32 {
        self.frames_since_cycle_start
            .saturating_add(self.frames_until_next_cost(speed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_recovery_repeats_the_boundary_extra_frame_late() {
        let mut timing = Fp24CostTiming::new(Fp24::REQUIRED_ONE);

        let lengths = collect_cycle_lengths(&mut timing, 34_964, Fp24::NORMAL_SPEED);

        assert_eq!(
            &lengths[..12],
            &[30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 31, 30]
        );
        assert_eq!(lengths[34_962], 31);
    }

    #[test]
    fn required_value_changes_place_the_first_extra_frame() {
        let mut slow = Fp24CostTiming::new(Fp24::REQUIRED_ONE.saturating_mul(2));
        let mut thirty_nine = Fp24CostTiming::new(Fp24::required_from_frames_per_cost(39));
        let mut forty_five = Fp24CostTiming::new(Fp24::required_from_frames_per_cost(45));

        assert_eq!(
            collect_cycle_lengths(&mut slow, 6, Fp24::NORMAL_SPEED),
            [60, 60, 60, 60, 60, 61]
        );
        assert_eq!(
            collect_cycle_lengths(&mut thirty_nine, 9, Fp24::NORMAL_SPEED),
            [39, 39, 39, 39, 39, 39, 39, 39, 40]
        );
        assert_eq!(
            collect_cycle_lengths(&mut forty_five, 8, Fp24::NORMAL_SPEED),
            [45, 45, 45, 45, 45, 45, 46, 45]
        );
    }

    fn collect_cycle_lengths(timing: &mut Fp24CostTiming, count: usize, speed: Fp24) -> Vec<i32> {
        let mut lengths = Vec::with_capacity(count);
        let mut frame_count = 0i32;
        let mut last_cycle_frame = 0i32;
        while lengths.len() < count {
            frame_count += 1;
            if timing.advance_one_frame(speed).recovered_cost {
                lengths.push(frame_count - last_cycle_frame);
                last_cycle_frame = frame_count;
            }
        }
        lengths
    }
}
