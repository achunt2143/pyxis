use smithay::backend::renderer::Renderer;
use smithay::backend::renderer::element::RenderElement;
use smithay::utils::Physical;
use anyhow::Result;

pub struct RenderState {
    pub render_counter: u64,
    pub damage_tracking: bool,
}

impl RenderState {
    pub fn new() -> Self {
        RenderState {
            render_counter: 0,
            damage_tracking: true,
        }
    }

    /// Render a frame
    pub fn render_frame(&mut self) -> Result<()> {
        self.render_counter += 1;
        
        if self.render_counter % 300 == 0 {
            tracing::debug!("Rendered {} frames", self.render_counter);
        }

        Ok(())
    }

    /// Get current render counter
    pub fn frame_count(&self) -> u64 {
        self.render_counter
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_state() {
        let mut render = RenderState::new();
        assert_eq!(render.frame_count(), 0);
        render.render_frame().unwrap();
        assert_eq!(render.frame_count(), 1);
    }
}
