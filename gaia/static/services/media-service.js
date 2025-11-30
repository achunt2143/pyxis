/**
 * Gaia Media Service
 * File: gaia/static/services/media-service.js
 * Purpose: Media file handling and playback
 */

class MediaService {
  constructor(apiService) {
    this.api = apiService;
    this.currentTrack = null;
    this.playlist = [];
    this.isPlaying = false;
  }

  /**
   * Get media metadata
   */
  async getMetadata(path) {
    return this.api.get(`/media/metadata`, { path });
  }

  /**
   * Play media
   */
  async play(path) {
    this.isPlaying = true;
    this.currentTrack = path;
    return { status: 'playing', path };
  }

  /**
   * Pause media
   */
  pause() {
    this.isPlaying = false;
    return { status: 'paused' };
  }

  /**
   * Stop media
   */
  stop() {
    this.isPlaying = false;
    this.currentTrack = null;
    return { status: 'stopped' };
  }

  /**
   * Set playlist
   */
  setPlaylist(tracks) {
    this.playlist = tracks;
    return { status: 'playlist_set', count: tracks.length };
  }

  /**
   * Next track
   */
  next() {
    return this.api.post(`/media/next`, {});
  }

  /**
   * Previous track
   */
  previous() {
    return this.api.post(`/media/previous`, {});
  }
}

export default MediaService;