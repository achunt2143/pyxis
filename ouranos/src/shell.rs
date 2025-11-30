use smithay::wayland::shell::xdg::XdgShellState;
use smithay::wayland::shell::wlr_layer::WlrLayerShellState;
use smithay::wayland::seat::WyseadState;
use smithay::wayland::data_device::DataDeviceState;
use smithay::wayland::output::OutputManagerState;
use smithay::wayland::dmabuf::DmabufState;
use smithay::wayland::compositor::CompositorState;
use smithay::wayland::shm::ShmState;
use anyhow::Result;

pub struct ShellState {
    pub xdg_shell: XdgShellState,
    pub layer_shell: WlrLayerShellState,
    pub data_device: DataDeviceState,
    pub output: OutputManagerState,
    pub compositor: CompositorState,
    pub shm: ShmState,
}

impl ShellState {
    pub fn new() -> Result<Self> {
        tracing::info!("Initializing Wayland shell protocols");

        Ok(ShellState {
            xdg_shell: XdgShellState::new(),
            layer_shell: WlrLayerShellState::new(),
            data_device: DataDeviceState::new(),
            output: OutputManagerState::new(),
            compositor: CompositorState::new(),
            shm: ShmState::new(),
        })
    }
}
