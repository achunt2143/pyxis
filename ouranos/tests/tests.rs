// Test harness and integration examples for GeckoOS Compositor
#[cfg(test)]
mod compositor_integration_tests {
    use geckoOS_compositor::*;

    /// Test: Window creation and destruction
    #[tokio::test]
    async fn test_window_lifecycle() {
        let mut state = GeckoOSState::new().await.unwrap();
        
        let mut metadata = WindowMetadata::default();
        metadata.app_id = "test-app".to_string();
        metadata.title = "Test Window".to_string();

        // Create window
        let window_id = state.create_window(metadata);
        assert!(state.get_window(window_id).is_some());

        // Verify window is in list
        let windows = state.list_windows();
        assert!(windows.iter().any(|(id, _)| *id == window_id));

        // Destroy window
        state.destroy_window(window_id);
        assert!(state.get_window(window_id).is_none());
    }

    /// Test: Window focus management
    #[tokio::test]
    async fn test_window_focus() {
        let mut state = GeckoOSState::new().await.unwrap();
        
        let metadata1 = WindowMetadata::default();
        let id1 = state.create_window(metadata1);

        let mut metadata2 = WindowMetadata::default();
        metadata2.app_id = "second-app".to_string();
        let id2 = state.create_window(metadata2);

        // Set focus to first window
        state.set_focus(Some(id1));
        let (focused_id, _) = state.get_focused_window().unwrap();
        assert_eq!(focused_id, id1);

        // Switch focus to second window
        state.set_focus(Some(id2));
        let (focused_id, _) = state.get_focused_window().unwrap();
        assert_eq!(focused_id, id2);

        // Clear focus
        state.set_focus(None);
        assert!(state.get_focused_window().is_none());
    }

    /// Test: Window state transitions
    #[tokio::test]
    async fn test_window_state_transitions() {
        let mut state = GeckoOSState::new().await.unwrap();
        
        let metadata = WindowMetadata::default();
        let window_id = state.create_window(metadata);

        // Normal → Maximized
        assert!(state.set_window_state(window_id, WindowState::Maximized));
        let window = state.get_window(window_id).unwrap();
        assert_eq!(window.state, WindowState::Maximized);

        // Maximized → Minimized
        assert!(state.set_window_state(window_id, WindowState::Minimized));
        let window = state.get_window(window_id).unwrap();
        assert_eq!(window.state, WindowState::Minimized);

        // Minimized → Fullscreen
        assert!(state.set_window_state(window_id, WindowState::Fullscreen));
        let window = state.get_window(window_id).unwrap();
        assert_eq!(window.state, WindowState::Fullscreen);

        // Fullscreen → Normal
        assert!(state.set_window_state(window_id, WindowState::Normal));
        let window = state.get_window(window_id).unwrap();
        assert_eq!(window.state, WindowState::Normal);
    }

    /// Test: Window geometry updates
    #[tokio::test]
    async fn test_window_geometry() {
        let mut state = GeckoOSState::new().await.unwrap();
        
        let metadata = WindowMetadata::default();
        let window_id = state.create_window(metadata);

        let new_geometry = WindowGeometry {
            x: 100,
            y: 200,
            width: 1024,
            height: 768,
        };

        assert!(state.set_window_geometry(window_id, new_geometry.clone()));
        let window = state.get_window(window_id).unwrap();
        assert_eq!(window.geometry.x, 100);
        assert_eq!(window.geometry.y, 200);
        assert_eq!(window.geometry.width, 1024);
        assert_eq!(window.geometry.height, 768);
    }

    /// Test: Multiple window management
    #[tokio::test]
    async fn test_multiple_windows() {
        let mut state = GeckoOSState::new().await.unwrap();
        
        let mut ids = Vec::new();
        for i in 0..5 {
            let mut metadata = WindowMetadata::default();
            metadata.app_id = format!("app-{}", i);
            ids.push(state.create_window(metadata));
        }

        // Verify all windows exist
        let windows = state.list_windows();
        assert_eq!(windows.len(), 5);

        // Destroy some windows
        state.destroy_window(ids[1]);
        state.destroy_window(ids[3]);

        let windows = state.list_windows();
        assert_eq!(windows.len(), 3);
    }

    /// Test: Compositor status reporting
    #[tokio::test]
    async fn test_compositor_status() {
        let mut state = GeckoOSState::new().await.unwrap();
        
        let metadata = WindowMetadata::default();
        state.create_window(metadata);

        let status = state.get_status();
        assert_eq!(status.window_count, 1);
        assert!(status.frame_count >= 0);
    }
}
