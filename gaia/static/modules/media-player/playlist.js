/**
 * Playlist Component
 * File: gaia/static/modules/media-player/playlist.js
 * Purpose: Playlist management
 */
class PlaylistPanel {
  constructor(options) {
    this.container = options.container;
    this.tracks = options.tracks || [];
    this.currentTrackIndex = 0;
  }

  render() {
    const html = `
      <div class="playlist-container">
        <h3>Playlist (${this.tracks.length} tracks)</h3>
        <div class="playlist-items">
          ${this.tracks.map((track, index) => `
            <div class="playlist-item ${index === this.currentTrackIndex ? 'active' : ''}">
              <span class="playlist-number">${index + 1}</span>
              <span class="playlist-title">${track.title}</span>
              <span class="playlist-artist">${track.artist}</span>
              <span class="playlist-duration">${track.duration}</span>
            </div>
          `).join('')}
        </div>
      </div>
    `;

    this.container.innerHTML = html;
    this.attachListeners();
  }

  attachListeners() {
    const items = this.container.querySelectorAll('.playlist-item');
    items.forEach((item, index) => {
      item.addEventListener('click', () => this.selectTrack(index));
    });
  }

  selectTrack(index) {
    this.currentTrackIndex = index;
    this.render();
    window.dispatchEvent(new CustomEvent('track:selected', { detail: this.tracks[index] }));
  }
}

export default PlaylistPanel;