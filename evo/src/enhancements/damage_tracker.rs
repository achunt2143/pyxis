//! Damage Tracking for Incremental Rendering
//!
//! Tracks modified regions to enable efficient partial screen updates

use std::collections::VecDeque;

/// Rectangle region
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Region {
    pub fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Region { x, y, width, height }
    }

    /// Get right boundary
    pub fn right(&self) -> i32 {
        self.x + self.width as i32
    }

    /// Get bottom boundary
    pub fn bottom(&self) -> i32 {
        self.y + self.height as i32
    }

    /// Check if regions intersect
    pub fn intersects(&self, other: &Region) -> bool {
        self.x < other.right()
            && other.x < self.right()
            && self.y < other.bottom()
            && other.y < self.bottom()
    }

    /// Compute intersection with another region
    pub fn intersection(&self, other: &Region) -> Option<Region> {
        if !self.intersects(other) {
            return None;
        }

        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());

        Some(Region {
            x,
            y,
            width: (right - x) as u32,
            height: (bottom - y) as u32,
        })
    }

    /// Merge with another region into bounding box
    pub fn union(&self, other: &Region) -> Region {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = self.right().max(other.right());
        let bottom = self.bottom().max(other.bottom());

        Region {
            x,
            y,
            width: (right - x) as u32,
            height: (bottom - y) as u32,
        }
    }

    /// Calculate area
    pub fn area(&self) -> u64 {
        (self.width as u64) * (self.height as u64)
    }

    /// Check if point is inside region
    pub fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && px < self.right() && py >= self.y && py < self.bottom()
    }
}

/// Damage tracker for incremental rendering
pub struct DamageTracker {
    current_damage: Vec<Region>,
    accumulated_damage: Vec<Region>,
    frame_history: VecDeque<Vec<Region>>,
    full_screen_width: u32,
    full_screen_height: u32,
    optimization_threshold: u64,
}

impl DamageTracker {
    pub fn new(width: u32, height: u32) -> Self {
        DamageTracker {
            current_damage: Vec::new(),
            accumulated_damage: Vec::new(),
            frame_history: VecDeque::with_capacity(60), // ~1 second at 60 FPS
            full_screen_width: width,
            full_screen_height: height,
            optimization_threshold: (width as u64 * height as u64) / 2, // 50% of screen
        }
    }

    /// Add damage region
    pub fn add_damage(&mut self, region: Region) {
        // Clamp to screen bounds
        let clamped = Region {
            x: region.x.max(0),
            y: region.y.max(0),
            width: region.width.min(self.full_screen_width),
            height: region.height.min(self.full_screen_height),
        };

        if clamped.area() > 0 {
            self.current_damage.push(clamped);
            tracing::trace!("Damage added: ({}, {}) {}x{}", clamped.x, clamped.y, clamped.width, clamped.height);
        }
    }

    /// Mark entire screen as damaged
    pub fn full_damage(&mut self) {
        self.current_damage.clear();
        self.current_damage.push(Region::new(
            0,
            0,
            self.full_screen_width,
            self.full_screen_height,
        ));
        tracing::debug!("Full screen damage marked");
    }

    /// Merge overlapping damage regions
    fn merge_regions(regions: &[Region]) -> Vec<Region> {
        if regions.is_empty() {
            return Vec::new();
        }

        let mut merged = Vec::<Region>::new();
        let mut sorted = regions.to_vec();
        sorted.sort_by_key(|r| (r.x, r.y));

        for region in sorted {
            if let Some(last) = merged.last_mut() {
                if last.intersects(&region) {
                    *last = last.union(&region);
                } else {
                    merged.push(region);
                }
            } else {
                merged.push(region);
            }
        }

        merged
    }

    /// Get accumulated damage for frame
    pub fn get_accumulated_damage(&self) -> Vec<Region> {
        Self::merge_regions(&self.accumulated_damage)
    }

    /// Commit current damage to accumulated
    pub fn commit_damage(&mut self) {
        self.accumulated_damage.extend(self.current_damage.drain(..));

        // Check if we should do full screen update
        let total_area: u64 = self.accumulated_damage.iter().map(|r| r.area()).sum();
        if total_area > self.optimization_threshold {
            tracing::debug!("Damage area {:.1}% of screen, switching to full update",
                (total_area as f64 / (self.full_screen_width as u64 * self.full_screen_height as u64) as f64) * 100.0);
            self.full_damage();
        }
    }

    /// Clear accumulated damage (after frame rendered)
    pub fn clear_damage(&mut self) {
        let merged = Self::merge_regions(&self.accumulated_damage);
        self.frame_history.push_back(merged);
        if self.frame_history.len() > 60 {
            self.frame_history.pop_front();
        }
        self.accumulated_damage.clear();
        tracing::trace!("Damage cleared");
    }

    /// Check if region needs update
    pub fn needs_update(&self, region: &Region) -> bool {
        for damage in &self.accumulated_damage {
            if damage.intersects(region) {
                return true;
            }
        }
        false
    }

    /// Get statistics
    pub fn get_stats(&self) -> DamageStats {
        let total_damaged: u64 = self.accumulated_damage.iter().map(|r| r.area()).sum();
        let total_screen = (self.full_screen_width as u64) * (self.full_screen_height as u64);
        let damage_percentage = if total_screen > 0 {
            (total_damaged as f64 / total_screen as f64) * 100.0
        } else {
            0.0
        };

        let avg_damaged = if self.frame_history.is_empty() {
            0
        } else {
            let sum: u64 = self.frame_history.iter()
                .map(|frame| frame.iter().map(|r| r.area()).sum::<u64>())
                .sum();
            sum / self.frame_history.len() as u64
        };

        DamageStats {
            current_regions: self.accumulated_damage.len(),
            current_area: total_damaged,
            damage_percentage,
            average_frame_damage: avg_damaged,
            frames_tracked: self.frame_history.len(),
        }
    }
}

/// Damage statistics
#[derive(Debug, Clone)]
pub struct DamageStats {
    pub current_regions: usize,
    pub current_area: u64,
    pub damage_percentage: f64,
    pub average_frame_damage: u64,
    pub frames_tracked: usize,
}

/// Dirty rectangle tracker with optimization
pub struct DirtyRectTracker {
    rects: Vec<Region>,
    max_rects: usize,
}

impl DirtyRectTracker {
    pub fn new(max_rects: usize) -> Self {
        DirtyRectTracker {
            rects: Vec::with_capacity(max_rects),
            max_rects,
        }
    }

    /// Add dirty rectangle
    pub fn add_rect(&mut self, region: Region) {
        if self.rects.len() < self.max_rects {
            self.rects.push(region);
        } else {
            // Merge into bounding box if too many
            let merged = self.rects.iter().copied().reduce(|a, b| a.union(&b)).unwrap();
            self.rects.clear();
            self.rects.push(merged);
            self.rects.push(region);
        }
    }

    /// Get bounding box of all dirty rects
    pub fn get_bounding_box(&self) -> Option<Region> {
        self.rects.iter().copied().reduce(|a, b| a.union(&b))
    }

    /// Clear all rects
    pub fn clear(&mut self) {
        self.rects.clear();
    }

    /// Get all rectangles
    pub fn get_rects(&self) -> &[Region] {
        &self.rects
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_region_creation() {
        let region = Region::new(0, 0, 100, 100);
        assert_eq!(region.width, 100);
        assert_eq!(region.height, 100);
        assert_eq!(region.area(), 10000);
    }

    #[test]
    fn test_region_intersection() {
        let r1 = Region::new(0, 0, 100, 100);
        let r2 = Region::new(50, 50, 100, 100);
        assert!(r1.intersects(&r2));
        
        let intersection = r1.intersection(&r2).unwrap();
        assert_eq!(intersection.x, 50);
        assert_eq!(intersection.y, 50);
        assert_eq!(intersection.width, 50);
        assert_eq!(intersection.height, 50);
    }

    #[test]
    fn test_region_union() {
        let r1 = Region::new(0, 0, 100, 100);
        let r2 = Region::new(50, 50, 100, 100);
        
        let union = r1.union(&r2);
        assert_eq!(union.x, 0);
        assert_eq!(union.y, 0);
        assert_eq!(union.right(), 150);
        assert_eq!(union.bottom(), 150);
    }

    #[test]
    fn test_damage_tracker() {
        let mut tracker = DamageTracker::new(1920, 1080);
        tracker.add_damage(Region::new(0, 0, 100, 100));
        tracker.commit_damage();
        
        let damage = tracker.get_accumulated_damage();
        assert_eq!(damage.len(), 1);
        
        tracker.clear_damage();
        let damage = tracker.get_accumulated_damage();
        assert_eq!(damage.len(), 0);
    }

    #[test]
    fn test_dirty_rect_tracker() {
        let mut tracker = DirtyRectTracker::new(10);
        tracker.add_rect(Region::new(0, 0, 100, 100));
        tracker.add_rect(Region::new(200, 200, 100, 100));
        
        let bbox = tracker.get_bounding_box().unwrap();
        assert_eq!(bbox.x, 0);
        assert_eq!(bbox.y, 0);
    }

    #[test]
    fn test_damage_stats() {
        let mut tracker = DamageTracker::new(1920, 1080);
        tracker.add_damage(Region::new(0, 0, 960, 540));
        tracker.commit_damage();
        
        let stats = tracker.get_stats();
        assert!(stats.damage_percentage > 0.0);
        assert!(stats.damage_percentage < 100.0);
    }
}
