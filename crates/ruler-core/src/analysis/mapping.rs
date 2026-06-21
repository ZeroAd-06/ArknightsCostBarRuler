/// Frame mapping with binary search.
/// Ported from Python utils.py::get_logical_frame_from_calibration()

/// Tolerance for approximate pixel width matching (same as Python)
pub const WIDTH_MATCH_TOLERANCE: i32 = 5;

/// A sorted calibration table for fast binary search lookup.
/// Built from the JSON pixel_map: { "pixel_width": logical_frame, ... }
#[derive(Clone, Debug)]
pub struct CalibrationTable {
    /// Sorted by pixel_width ascending
    entries: Vec<(i32, i32)>,
    pub total_frames: i32,
}

impl CalibrationTable {
    /// Build a calibration table from a pixel_map dict (string keys → int values).
    pub fn from_pixel_map(
        pixel_map: &std::collections::HashMap<String, i32>,
        total_frames: i32,
    ) -> Self {
        let mut entries: Vec<(i32, i32)> = pixel_map
            .iter()
            .filter_map(|(k, &v)| k.parse::<i32>().ok().map(|pk| (pk, v)))
            .collect();
        entries.sort_by_key(|&(pk, _)| pk);
        CalibrationTable {
            entries,
            total_frames,
        }
    }

    /// Look up the logical frame for a given pixel width.
    /// Uses binary search for O(log n) lookup instead of Python's O(n) linear scan.
    pub fn lookup(&self, pixel_width: i32) -> Option<i32> {
        if self.entries.is_empty() {
            return None;
        }

        let pos = self.entries.partition_point(|&(pk, _)| pk < pixel_width);
        let mut best_diff = i32::MAX;
        let mut best_frame: Option<i32> = None;

        for idx in pos.saturating_sub(1)..=(pos.min(self.entries.len() - 1)) {
            let (pk, frame) = self.entries[idx];
            let diff = (pk - pixel_width).abs();
            if diff < best_diff {
                best_diff = diff;
                best_frame = Some(frame);
            }
        }

        if best_diff <= WIDTH_MATCH_TOLERANCE {
            best_frame
        } else {
            None
        }
    }

    pub fn lookup_phase(&self, pixel_width: i32) -> Option<f64> {
        if self.entries.is_empty() || self.total_frames <= 0 {
            return None;
        }

        Some(
            (self.lookup_interpolated_frame(pixel_width)? / self.total_frames as f64)
                .clamp(0.0, 1.0),
        )
    }

    pub fn lookup_interpolated_frame(&self, pixel_width: i32) -> Option<f64> {
        if self.entries.is_empty() {
            return None;
        }

        let pos = self.entries.partition_point(|&(pk, _)| pk < pixel_width);
        if pos < self.entries.len() && self.entries[pos].0 == pixel_width {
            return Some(self.entries[pos].1 as f64);
        }

        if pos == 0 {
            let (width, frame) = self.entries[0];
            return ((width - pixel_width).abs() <= WIDTH_MATCH_TOLERANCE).then_some(frame as f64);
        }

        if pos >= self.entries.len() {
            let (width, frame) = self.entries[self.entries.len() - 1];
            return ((width - pixel_width).abs() <= WIDTH_MATCH_TOLERANCE).then_some(frame as f64);
        }

        let (lower_width, lower_frame) = self.entries[pos - 1];
        let (upper_width, upper_frame) = self.entries[pos];
        if lower_width == upper_width {
            return Some(lower_frame as f64);
        }

        let width_ratio = (pixel_width - lower_width) as f64 / (upper_width - lower_width) as f64;
        let interpolated_frame =
            lower_frame as f64 + width_ratio * (upper_frame - lower_frame) as f64;
        Some(interpolated_frame)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_table() -> CalibrationTable {
        let mut map = std::collections::HashMap::new();
        map.insert("0".to_string(), 0);
        map.insert("5".to_string(), 1);
        map.insert("10".to_string(), 2);
        map.insert("15".to_string(), 3);
        map.insert("20".to_string(), 4);
        CalibrationTable::from_pixel_map(&map, 30)
    }

    #[test]
    fn test_exact_match() {
        let table = make_test_table();
        assert_eq!(table.lookup(10), Some(2));
        assert_eq!(table.lookup(0), Some(0));
        assert_eq!(table.lookup(20), Some(4));
    }

    #[test]
    fn test_approximate_match() {
        let table = make_test_table();
        assert_eq!(table.lookup(12), Some(2));
        assert_eq!(table.lookup(8), Some(2));
    }

    #[test]
    fn test_beyond_tolerance() {
        let table = make_test_table();
        assert_eq!(table.lookup(50), None);
    }

    #[test]
    fn test_empty_table() {
        let table = CalibrationTable::from_pixel_map(&std::collections::HashMap::new(), 0);
        assert_eq!(table.lookup(10), None);
    }

    #[test]
    fn test_boundary_tolerance() {
        let table = make_test_table();
        assert_eq!(table.lookup(25), Some(4));
        assert_eq!(table.lookup(26), None);
    }

    #[test]
    fn lookup_phase_interpolates_between_sparse_widths() {
        let mut map = std::collections::HashMap::new();
        map.insert("0".to_string(), 0);
        map.insert("3".to_string(), 2);
        map.insert("5".to_string(), 4);
        map.insert("7".to_string(), 7);
        let table = CalibrationTable::from_pixel_map(&map, 8);

        let phase = table.lookup_phase(1).unwrap();
        assert!((phase - (2.0 / 3.0) / 8.0).abs() < 1e-9);

        let phase = table.lookup_phase(4).unwrap();
        assert!((phase - 3.0 / 8.0).abs() < 1e-9);
    }
}
