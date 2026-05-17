/// Frame mapping with binary search.
/// Ported from Python utils.py::get_logical_frame_from_calibration()

/// Tolerance for approximate pixel width matching (same as Python)
const TOLERANCE: i32 = 5;

/// A sorted calibration table for fast binary search lookup.
/// Built from the JSON pixel_map: { "pixel_width": logical_frame, ... }
#[derive(Clone, Debug)]
pub struct CalibrationTable {
    /// Sorted by pixel_width ascending
    entries: Vec<(i32, i32)>, // (pixel_width, logical_frame)
    pub total_frames: i32,
}

impl CalibrationTable {
    /// Build a calibration table from a pixel_map dict (string keys → int values).
    /// The entries are sorted by pixel_width for binary search.
    pub fn from_pixel_map(pixel_map: &std::collections::HashMap<String, i32>, total_frames: i32) -> Self {
        let mut entries: Vec<(i32, i32)> = pixel_map
            .iter()
            .filter_map(|(k, &v)| k.parse::<i32>().ok().map(|pk| (pk, v)))
            .collect();
        entries.sort_by_key(|&(pk, _)| pk);
        CalibrationTable { entries, total_frames }
    }

    /// Look up the logical frame for a given pixel width.
    /// Uses binary search for O(log n) lookup instead of Python's O(n) linear scan.
    /// Returns None if no match within tolerance.
    pub fn lookup(&self, pixel_width: i32) -> Option<i32> {
        if self.entries.is_empty() {
            return None;
        }

        // Binary search for the closest pixel width
        let pos = self.entries.partition_point(|&(pk, _)| pk < pixel_width);

        // Check the element at pos and pos-1 to find the closest
        let mut best_diff = i32::MAX;
        let mut best_frame: Option<i32> = None;

        for &(_, frame) in self.entries.iter().take(pos + 1).skip(pos.saturating_sub(1)) {
            // already handled below
            let _ = frame;
        }

        // Check positions around the binary search result
        for idx in pos.saturating_sub(1)..=(pos.min(self.entries.len() - 1)) {
            let (pk, frame) = self.entries[idx];
            let diff = (pk - pixel_width).abs();
            if diff < best_diff {
                best_diff = diff;
                best_frame = Some(frame);
            }
        }

        if best_diff <= TOLERANCE {
            best_frame
        } else {
            None
        }
    }

    /// Get the number of entries in the calibration table
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
        // 12 is closest to 10 (diff=2 < tolerance=5)
        assert_eq!(table.lookup(12), Some(2));
        // 8 is closest to 10 (diff=2 < tolerance=5)
        assert_eq!(table.lookup(8), Some(2));
    }

    #[test]
    fn test_beyond_tolerance() {
        let table = make_test_table();
        // 50 is far from any entry
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
        // 15+5=20 → matches 20 (diff=5, exactly at tolerance)
        assert_eq!(table.lookup(25), Some(4));
        // 20+6=26 → beyond tolerance from 20
        assert_eq!(table.lookup(26), None);
    }
}
