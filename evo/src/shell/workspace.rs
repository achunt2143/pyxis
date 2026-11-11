//! Complete workspace management for GeckoOS compositor
//! 
//! This module provides full workspace functionality including:
//! - Window-to-workspace binding
//! - Space synchronization (hide/show windows)
//! - Per-workspace window state persistence
//! - Z-order management per workspace
//! - Workspace switching with proper rendering updates

use std::collections::HashMap;
use smithay::utils::{Logical, Point, Size};
use tracing::{debug, info, warn};

use crate::shell::WindowElement;

/// Unique identifier for workspaces
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WorkspaceId(pub u32);

/// Window layout mode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowLayout {
    Floating,
    Tiled,
    Maximized,
    Fullscreen,
}

/// Per-window state stored in workspace
#[derive(Debug, Clone)]
pub struct WindowState {
    /// Window location in logical coordinates
    pub location: Point<i32, Logical>,
    /// Window size
    pub size: Size<i32, Logical>,
    /// Current layout mode
    pub layout: WindowLayout,
    /// Z-order index (higher = on top)
    pub z_index: usize,
    /// Whether window is minimized (hidden but tracked)
    pub minimized: bool,
    /// Whether window is sticky (visible on all workspaces)
    pub sticky: bool,
    /// User explicitly set position
    pub user_positioned: bool,
    /// User explicitly set size
    pub user_resized: bool,
    /// The location last announced as `WindowMoved`, so a grab that ends
    /// where it began (or ends twice) says nothing.
    pub reported_location: Option<Point<i32, Logical>>,
}

impl WindowState {
    /// Take the window off the screen. `location` is where the space has it
    /// now, which is the truth for anything mapped; the record is only ever
    /// stale for a window that has been hidden since.
    pub fn minimize(&mut self, location: Option<Point<i32, Logical>>) {
        if let Some(location) = location {
            self.location = location;
        }
        self.minimized = true;
    }

    /// Bring it back: the place and layout it had when it went away. The
    /// layout is what a restored window must be put back *as*: a maximized
    /// window is still maximized, a fullscreen one is fullscreen again.
    pub fn restore(&mut self) -> (Point<i32, Logical>, WindowLayout) {
        self.minimized = false;
        (self.location, self.layout)
    }
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            location: (0, 0).into(),
            size: (800, 600).into(),
            layout: WindowLayout::Floating,
            z_index: 0,
            minimized: false,
            sticky: false,
            user_positioned: false,
            user_resized: false,
            reported_location: None,
        }
    }
}

/// A single workspace containing windows and their state
#[derive(Debug)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub name: String,
    /// Windows in Z-order (index 0 = bottom, last = top)
    windows: Vec<WindowElement>,
    /// Per-window state (location, size, etc)
    window_states: HashMap<WindowElement, WindowState>,
    /// Next Z-index to assign
    next_z_index: usize,
}

impl Workspace {
    fn new(id: WorkspaceId, name: String) -> Self {
        Self {
            id,
            name,
            windows: Vec::new(),
            window_states: HashMap::new(),
            next_z_index: 0,
        }
    }

    /// Add window to workspace with default state
    pub fn add_window(&mut self, window: WindowElement, location: Point<i32, Logical>) {
        if self.windows.contains(&window) {
            warn!("Window already in workspace {:?}", self.id);
            return;
        }

        let state = WindowState {
            location,
            z_index: self.next_z_index,
            ..Default::default()
        };
        
        self.next_z_index += 1;
        self.windows.push(window.clone());
        self.window_states.insert(window, state);
        
        debug!(
            "Added window to workspace {:?}, total windows: {}",
            self.id,
            self.windows.len()
        );
    }

    /// Add window with specific state
    pub fn add_window_with_state(&mut self, window: WindowElement, state: WindowState) {
        if self.windows.contains(&window) {
            warn!("Window already in workspace {:?}", self.id);
            return;
        }

        self.windows.push(window.clone());
        self.window_states.insert(window, state);
        
        debug!("Added window with custom state to workspace {:?}", self.id);
    }

    /// Remove window from workspace
    pub fn remove_window(&mut self, window: &WindowElement) -> Option<WindowState> {
        if let Some(pos) = self.windows.iter().position(|w| w == window) {
            self.windows.remove(pos);
            let state = self.window_states.remove(window);
            debug!(
                "Removed window from workspace {:?}, remaining: {}",
                self.id,
                self.windows.len()
            );
            state
        } else {
            None
        }
    }

    /// Get window state
    pub fn window_state(&self, window: &WindowElement) -> Option<&WindowState> {
        self.window_states.get(window)
    }

    /// Get mutable window state
    pub fn window_state_mut(&mut self, window: &WindowElement) -> Option<&mut WindowState> {
        self.window_states.get_mut(window)
    }

    /// Update window state
    pub fn update_window_state(&mut self, window: &WindowElement, state: WindowState) {
        if self.windows.contains(window) {
            self.window_states.insert(window.clone(), state);
        }
    }

    /// Get all windows in Z-order (bottom to top)
    pub fn windows(&self) -> &[WindowElement] {
        &self.windows
    }

    /// Get windows in render order (top to bottom for rendering)
    pub fn windows_render_order(&self) -> impl Iterator<Item = &WindowElement> {
        self.windows.iter().rev()
    }

    /// Raise window to top of Z-order
    pub fn raise_window(&mut self, window: &WindowElement) {
        if let Some(pos) = self.windows.iter().position(|w| w == window) {
            let window = self.windows.remove(pos);
            self.windows.push(window.clone());
            
            // Update Z-index
            if let Some(state) = self.window_states.get_mut(&window) {
                state.z_index = self.next_z_index;
                self.next_z_index += 1;
            }
            
            debug!("Raised window in workspace {:?}", self.id);
        }
    }

    /// Lower window to bottom of Z-order
    pub fn lower_window(&mut self, window: &WindowElement) {
        if let Some(pos) = self.windows.iter().position(|w| w == window) {
            let window = self.windows.remove(pos);
            self.windows.insert(0, window.clone());
            
            // Update Z-index
            if let Some(state) = self.window_states.get_mut(&window) {
                state.z_index = 0;
            }
            
            debug!("Lowered window in workspace {:?}", self.id);
        }
    }

    /// Get visible windows (not minimized)
    pub fn visible_windows(&self) -> impl Iterator<Item = &WindowElement> + '_ {
        self.windows.iter().filter(|w| {
            self.window_states
                .get(w)
                .map(|s| !s.minimized)
                .unwrap_or(true)
        })
    }

    /// Check if window is in this workspace
    pub fn contains_window(&self, window: &WindowElement) -> bool {
        self.windows.contains(window)
    }

    /// Get window count
    pub fn window_count(&self) -> usize {
        self.windows.len()
    }

    /// Get visible window count
    pub fn visible_window_count(&self) -> usize {
        self.visible_windows().count()
    }
}

/// Workspace manager for the compositor
#[derive(Debug)]
pub struct WorkspaceManager {
    workspaces: HashMap<WorkspaceId, Workspace>,
    active_workspace: WorkspaceId,
    next_id: u32,
}

impl WorkspaceManager {
    /// Create new workspace manager with one default workspace
    pub fn new() -> Self {
        let mut workspaces = HashMap::new();
        let first = Workspace::new(WorkspaceId(0), "Workspace 1".to_string());
        workspaces.insert(WorkspaceId(0), first);

        info!("Workspace manager initialized with 1 workspace");

        Self {
            workspaces,
            active_workspace: WorkspaceId(0),
            next_id: 1,
        }
    }

    /// Create a new workspace
    pub fn create_workspace(&mut self, name: String) -> WorkspaceId {
        let id = WorkspaceId(self.next_id);
        self.next_id += 1;

        let workspace = Workspace::new(id, name.clone());
        self.workspaces.insert(id, workspace);

        info!("Created workspace {:?}: {}", id, name);
        id
    }

    /// Create multiple workspaces at once
    pub fn create_workspaces(&mut self, count: u32, name_prefix: &str) -> Vec<WorkspaceId> {
        let mut ids = Vec::new();
        for i in 0..count {
            let name = format!("{} {}", name_prefix, self.next_id);
            ids.push(self.create_workspace(name));
        }
        ids
    }

    /// Get active workspace ID
    pub fn active_workspace_id(&self) -> WorkspaceId {
        self.active_workspace
    }

    /// Get active workspace
    pub fn active_workspace(&self) -> &Workspace {
        self.workspaces.get(&self.active_workspace).unwrap()
    }

    /// Get mutable active workspace
    pub fn active_workspace_mut(&mut self) -> &mut Workspace {
        self.workspaces.get_mut(&self.active_workspace).unwrap()
    }

    /// Get workspace by ID
    pub fn workspace(&self, id: WorkspaceId) -> Option<&Workspace> {
        self.workspaces.get(&id)
    }

    /// Get mutable workspace by ID
    pub fn workspace_mut(&mut self, id: WorkspaceId) -> Option<&mut Workspace> {
        self.workspaces.get_mut(&id)
    }

    /// Switch to a different workspace
    pub fn switch_to(&mut self, workspace_id: WorkspaceId) -> bool {
        if self.workspaces.contains_key(&workspace_id) {
            if self.active_workspace != workspace_id {
                info!(
                    "Switching workspace: {:?} -> {:?}",
                    self.active_workspace, workspace_id
                );
                self.active_workspace = workspace_id;
            }
            true
        } else {
            warn!("Attempted to switch to non-existent workspace {:?}", workspace_id);
            false
        }
    }

    /// Switch to next workspace (wraps around)
    pub fn switch_to_next(&mut self) {
        let ids: Vec<_> = self.workspaces.keys().copied().collect();
        if ids.len() <= 1 {
            return;
        }

        let current_idx = ids.iter().position(|&id| id == self.active_workspace).unwrap();
        let next_idx = (current_idx + 1) % ids.len();
        self.switch_to(ids[next_idx]);
    }

    /// Switch to previous workspace (wraps around)
    pub fn switch_to_previous(&mut self) {
        let ids: Vec<_> = self.workspaces.keys().copied().collect();
        if ids.len() <= 1 {
            return;
        }

        let current_idx = ids.iter().position(|&id| id == self.active_workspace).unwrap();
        let prev_idx = if current_idx == 0 {
            ids.len() - 1
        } else {
            current_idx - 1
        };
        self.switch_to(ids[prev_idx]);
    }

    /// Add window to active workspace
    pub fn add_window(&mut self, window: WindowElement, location: Point<i32, Logical>) {
        self.active_workspace_mut().add_window(window, location);
    }

    /// Add window to specific workspace
    pub fn add_window_to_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        window: WindowElement,
        location: Point<i32, Logical>,
    ) -> bool {
        if let Some(workspace) = self.workspaces.get_mut(&workspace_id) {
            workspace.add_window(window, location);
            true
        } else {
            false
        }
    }

    /// Move window from one workspace to another
    pub fn move_window_to_workspace(
        &mut self,
        window: &WindowElement,
        target_workspace: WorkspaceId,
    ) -> bool {
        // Find and remove window from current workspace
        let state = self
            .workspaces
            .values_mut()
            .find_map(|ws| ws.remove_window(window));

        if let Some(state) = state {
            // Add to target workspace with preserved state
            if let Some(workspace) = self.workspaces.get_mut(&target_workspace) {
                workspace.add_window_with_state(window.clone(), state);
                info!(
                    "Moved window to workspace {:?}",
                    target_workspace
                );
                return true;
            }
        }

        false
    }

    /// Remove window from all workspaces
    pub fn remove_window(&mut self, window: &WindowElement) {
        for workspace in self.workspaces.values_mut() {
            workspace.remove_window(window);
        }
        debug!("Removed window from all workspaces");
    }

    /// Get workspace containing a window
    pub fn workspace_for_window(&self, window: &WindowElement) -> Option<WorkspaceId> {
        for (id, workspace) in &self.workspaces {
            if workspace.contains_window(window) {
                return Some(*id);
            }
        }
        None
    }

    /// Get all workspace IDs
    pub fn workspace_ids(&self) -> Vec<WorkspaceId> {
        self.workspaces.keys().copied().collect()
    }

    /// Get total workspace count
    pub fn workspace_count(&self) -> usize {
        self.workspaces.len()
    }

    /// Get all windows on active workspace
    pub fn active_windows(&self) -> &[WindowElement] {
        self.active_workspace().windows()
    }

    /// Get all visible windows on active workspace
    pub fn active_visible_windows(&self) -> Vec<&WindowElement> {
        self.active_workspace().visible_windows().collect()
    }

    /// Get all sticky windows (visible on all workspaces)
    pub fn sticky_windows(&self) -> Vec<&WindowElement> {
        self.active_workspace()
            .windows()
            .iter()
            .filter(|w| {
                self.active_workspace()
                    .window_state(w)
                    .map(|s| s.sticky)
                    .unwrap_or(false)
            })
            .collect()
    }

    /// Toggle window sticky status
    pub fn toggle_sticky(&mut self, window: &WindowElement) {
        if let Some(ws_id) = self.workspace_for_window(window) {
            if let Some(workspace) = self.workspaces.get_mut(&ws_id) {
                if let Some(state) = workspace.window_state_mut(window) {
                    state.sticky = !state.sticky;
                    info!("Toggled sticky for window: {}", state.sticky);
                }
            }
        }
    }

    /// Minimize window. `location` is where the space has it right now.
    pub fn minimize_window(&mut self, window: &WindowElement, location: Option<Point<i32, Logical>>) {
        if let Some(state) = self.state_mut(window) {
            state.minimize(location);
            debug!("Minimized window");
        }
    }

    /// Unminimize window: the place and layout to put it back as.
    pub fn unminimize_window(
        &mut self,
        window: &WindowElement,
    ) -> Option<(Point<i32, Logical>, WindowLayout)> {
        let restored = self.state_mut(window).map(WindowState::restore);
        debug!("Unminimized window");
        restored
    }

    /// Raise window in its workspace
    pub fn raise_window(&mut self, window: &WindowElement) {
        if let Some(ws_id) = self.workspace_for_window(window) {
            if let Some(workspace) = self.workspaces.get_mut(&ws_id) {
                workspace.raise_window(window);
            }
        }
    }

    /// Lower window in its workspace
    pub fn lower_window(&mut self, window: &WindowElement) {
        if let Some(ws_id) = self.workspace_for_window(window) {
            if let Some(workspace) = self.workspaces.get_mut(&ws_id) {
                workspace.lower_window(window);
            }
        }
    }

    /// Update window location (marks as user-positioned)
    pub fn set_window_location(&mut self, window: &WindowElement, location: Point<i32, Logical>) {
        if let Some(ws_id) = self.workspace_for_window(window) {
            if let Some(workspace) = self.workspaces.get_mut(&ws_id) {
                if let Some(state) = workspace.window_state_mut(window) {
                    state.location = location;
                    state.user_positioned = true;
                }
            }
        }
    }

    /// Update window size (marks as user-resized)
    pub fn set_window_size(&mut self, window: &WindowElement, size: Size<i32, Logical>) {
        if let Some(ws_id) = self.workspace_for_window(window) {
            if let Some(workspace) = self.workspaces.get_mut(&ws_id) {
                if let Some(state) = workspace.window_state_mut(window) {
                    state.size = size;
                    state.user_resized = true;
                }
            }
        }
    }

    /// Record where a window is without calling it a user's choice. Every
    /// position change funnels through `EvoState::place_window`, which is
    /// the only caller.
    pub fn record_location(&mut self, window: &WindowElement, location: Point<i32, Logical>) {
        if let Some(state) = self.state_mut(window) {
            state.location = location;
        }
    }

    /// Record a window's layout. Returns the one it had, if it is tracked.
    pub fn set_layout(&mut self, window: &WindowElement, layout: WindowLayout) -> Option<WindowLayout> {
        let state = self.state_mut(window)?;
        Some(std::mem::replace(&mut state.layout, layout))
    }

    /// Note the location last announced as `WindowMoved`; returns the one
    /// announced before.
    pub fn set_reported_location(
        &mut self,
        window: &WindowElement,
        location: Point<i32, Logical>,
    ) -> Option<Point<i32, Logical>> {
        let state = self.state_mut(window)?;
        state.reported_location.replace(location)
    }

    /// The record for `window`, in whichever workspace holds it.
    pub fn state(&self, window: &WindowElement) -> Option<&WindowState> {
        self.workspaces.values().find_map(|ws| ws.window_state(window))
    }

    fn state_mut(&mut self, window: &WindowElement) -> Option<&mut WindowState> {
        self.workspaces
            .values_mut()
            .find_map(|ws| ws.window_state_mut(window))
    }

    /// Whether `window` is tracked and minimized.
    pub fn is_minimized(&self, window: &WindowElement) -> bool {
        self.state(window).is_some_and(|s| s.minimized)
    }

    /// Every window on every workspace, mapped or not. A minimized window is
    /// out of the space but is still in here: this is how it is found again.
    pub fn all_windows(&self) -> impl Iterator<Item = &WindowElement> + '_ {
        self.workspaces.values().flat_map(|ws| ws.windows().iter())
    }

    /// Get window location from workspace state
    pub fn window_location(&self, window: &WindowElement) -> Option<Point<i32, Logical>> {
        let ws_id = self.workspace_for_window(window)?;
        let workspace = self.workspaces.get(&ws_id)?;
        workspace.window_state(window).map(|s| s.location)
    }

    /// Delete workspace (moves windows to active workspace)
    pub fn delete_workspace(&mut self, workspace_id: WorkspaceId) -> bool {
        if workspace_id == self.active_workspace {
            warn!("Cannot delete active workspace");
            return false;
        }

        if self.workspaces.len() <= 1 {
            warn!("Cannot delete last workspace");
            return false;
        }

        if let Some(workspace) = self.workspaces.remove(&workspace_id) {
            // Move all windows to active workspace
            for window in workspace.windows {
                if let Some(state) = workspace.window_states.get(&window) {
                    self.active_workspace_mut()
                        .add_window_with_state(window, state.clone());
                }
            }
            info!("Deleted workspace {:?}", workspace_id);
            true
        } else {
            false
        }
    }

    /// Rename workspace
    pub fn rename_workspace(&mut self, workspace_id: WorkspaceId, new_name: String) -> bool {
        if let Some(workspace) = self.workspaces.get_mut(&workspace_id) {
            workspace.name = new_name.clone();
            info!("Renamed workspace {:?} to '{}'", workspace_id, new_name);
            true
        } else {
            false
        }
    }
}

impl Default for WorkspaceManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_returns_the_window_where_it_was_moved_to() {
        // Born at (40, 60), dragged to (300, 220), then minimized. The record
        // used to hold only the birth location.
        let mut state = WindowState { location: (40, 60).into(), ..Default::default() };
        state.location = (300, 220).into();
        state.minimize(Some((300, 220).into()));
        assert!(state.minimized);
        assert_eq!(state.restore(), ((300, 220).into(), WindowLayout::Floating));
        assert!(!state.minimized);
    }

    #[test]
    fn minimize_takes_the_spaces_location_over_a_stale_record() {
        let mut state = WindowState { location: (40, 60).into(), ..Default::default() };
        state.minimize(Some((500, 90).into()));
        assert_eq!(state.restore().0, (500, 90).into());
        // A window that is already hidden has no space location to offer.
        state.minimize(None);
        assert_eq!(state.restore().0, (500, 90).into());
    }

    #[test]
    fn a_maximized_window_comes_back_maximized_at_the_output_origin() {
        let mut state = WindowState { location: (40, 60).into(), ..Default::default() };
        state.location = (0, 0).into();
        state.layout = WindowLayout::Maximized;
        state.minimize(Some((0, 0).into()));
        assert_eq!(state.restore(), ((0, 0).into(), WindowLayout::Maximized));
    }

    #[test]
    fn test_workspace_creation() {
        let mut manager = WorkspaceManager::new();
        assert_eq!(manager.workspace_count(), 1);

        let ws2 = manager.create_workspace("Test Workspace".to_string());
        assert_eq!(manager.workspace_count(), 2);
        assert!(manager.workspace(ws2).is_some());
    }

    #[test]
    fn test_workspace_switching() {
        let mut manager = WorkspaceManager::new();
        let ws2 = manager.create_workspace("Workspace 2".to_string());

        assert!(manager.switch_to(ws2));
        assert_eq!(manager.active_workspace_id(), ws2);

        manager.switch_to_next();
        assert_eq!(manager.active_workspace_id(), WorkspaceId(0));
    }
}
