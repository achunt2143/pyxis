/// Gaia Shell state
#[derive(Debug, Clone)]
pub struct ShellState {
    pub initialized: bool,
    pub running: bool,
    pub active_window: Option<u32>,
    pub open_apps: usize,
}

impl ShellState {
    pub fn new() -> Self {
        ShellState {
            initialized: false,
            running: false,
            active_window: None,
            open_apps: 0,
        }
    }
}

impl Default for ShellState {
    fn default() -> Self {
        Self::new()
    }
}