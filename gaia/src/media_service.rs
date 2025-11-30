// ============================================================================
// MEDIA SERVICE MODULE
// File: gaia/src/media_service.rs
// Purpose: Media file handling and playback
// ============================================================================

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MediaFile {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub duration: u32,
    pub path: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PlaybackState {
    pub is_playing: bool,
    pub current_track: Option<String>,
    pub position: u32,
}

pub struct MediaService {
    playlist: Vec<MediaFile>,
    playback_state: PlaybackState,
}

impl MediaService {
    pub fn new() -> Self {
        info!("🎵 Initializing Media Service");
        
        MediaService {
            playlist: vec![],
            playback_state: PlaybackState {
                is_playing: false,
                current_track: None,
                position: 0,
            },
        }
    }

    pub fn add_to_playlist(&mut self, file: MediaFile) {
        debug!("➕ Adding to playlist: {}", file.title);
        self.playlist.push(file);
    }

    pub fn play(&mut self, track_id: &str) -> Result<(), String> {
        debug!("▶️ Playing track: {}", track_id);
        
        self.playback_state.is_playing = true;
        self.playback_state.current_track = Some(track_id.to_string());
        Ok(())
    }

    pub fn pause(&mut self) {
        debug!("⏸️ Pausing playback");
        self.playback_state.is_playing = false;
    }

    pub fn get_playlist(&self) -> Vec<MediaFile> {
        self.playlist.clone()
    }

    pub fn get_playback_state(&self) -> PlaybackState {
        self.playback_state.clone()
    }
}